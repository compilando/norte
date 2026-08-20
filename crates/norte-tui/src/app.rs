//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::{Lang, t, ta};
use norte_proto::{Error, VPath};

mod banners;
mod caps;
mod compare;
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
mod prompts;
mod session;
mod trail;

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

/// El formato de archivo que sugiere un NOMBRE, entre los que se saben
/// ESCRIBIR (#132).
///
/// Azúcar de presentación, igual que el mapa de `nav::archive_root_for`: lo
/// que decide es el campo explícito del wire, y esto solo rellena el diálogo
/// con lo que el usuario acaba de teclear. `rar` no está — se delega y solo
/// para leer (ADR 0056)—, así que un `.rar` cae en `None` y el diálogo lo dice
/// en vez de empaquetar un zip con nombre de rar.
#[must_use]
pub fn format_by_name(name: &[u8]) -> Option<norte_proto::methods::ArchiveFormat> {
    use norte_proto::methods::ArchiveFormat as F;
    let ends = |suf: &[u8]| {
        name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
    };
    if ends(b".tar.gz") || ends(b".tgz") {
        return Some(F::TarGz);
    }
    if ends(b".tar") {
        return Some(F::Tar);
    }
    if ends(b".zip") {
        return Some(F::Zip);
    }
    None
}

/// Un tamaño con sufijo (`4096`, `10M`, `1G`) en bytes, o `None` si no se
/// entiende (#132).
///
/// Sufijos binarios, que es lo que significan en un gestor de ficheros: `M` es
/// 1 MiB y no un millón. Sin sufijo son bytes.
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.as_bytes()[s.len() - 1].to_ascii_uppercase() {
        b'K' => (&s[..s.len() - 1], 1024_u64),
        b'M' => (&s[..s.len() - 1], 1024 * 1024),
        b'G' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().ok()?;
    n.checked_mul(mult).filter(|v| *v > 0)
}

/// El estado del run (`CompareState`) y el panel abierto (`CompareView`)
/// viven en [`norte_frontend::compare`] (#158): la GUI necesita exactamente
/// esta máquina y no una reimplementada, que es como el CLI (fase A) y la
/// tool MCP (fase B) se equivocaron cada uno por su lado — ambos dieron por
/// completa una respuesta a la que le faltaban lotes. Ver
/// [`CompareState::Incomplete`] para la razón de que el cierre del canal no
/// baste.
pub use norte_frontend::compare::{CompareState, CompareView};

/// Las dos raíces de una sincronización y cómo se leen sus nombres.
///
/// Vive en [`norte_frontend::sync`] desde #161, con la función que las decide:
/// la GUI llegó a tener la MISMA regla escrita a mano (su brazo «el pane con
/// foco es el origen»), y dos copias de «qué árbol se sobrescribe» es la clase
/// de divergencia que produce un plan perfectamente plausible sobre el árbol
/// equivocado.

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
/// hay «programa» que sondear en el PATH — el argv sale de `$SHELL` o de una
/// línea que el usuario escribió, y un `$SHELL` roto se dice con el error del
/// spawn, no con una sonda que adivinaría lo mismo.
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

/// How many connection degradations `App` retains at once.
///
/// #44 held ONE, as an `Option<String>`, so it was bounded by construction; a
/// collection keyed on a scheme the WIRE supplies is not, and the daemon is
/// not the only thing that can send those notifications. Thirty-two is far
/// more than the seven schemes that exist, so the cap can only ever be reached
/// by something abnormal — and when it is, the oldest report is dropped and the
/// reader keeps the ones that just arrived. Same discipline as the daemon's
/// retained listings and its approvals cap.
const DEGRADED_MAX: usize = 32;

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
    /// Cuándo se tocó cada hueco por última vez (epoch ms), para la barrida
    /// por edad. Se guarda en vez de sellarse al capturar porque capturar no
    /// es tocar: dos capturas seguidas de la misma pantalla tienen que dar el
    /// mismo documento.
    touched: std::collections::HashMap<u32, u64>,
    /// El cursor que traía la sesión, hasta que llegue el listado que lo puede
    /// colocar: sobre un pane vacío, poner el cursor en la fila 12 es ponerlo
    /// en la 0.
    cursors: std::collections::HashMap<u32, u64>,
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
    /// El selector de conexiones (#140), si está abierto.
    pub connections_picker: Option<norte_frontend::connections_picker::ConnectionsPicker>,
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
    degraded: std::collections::VecDeque<norte_proto::methods::ConnectionDegraded>,
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
    pub pending_disconnect_home: Option<VPath>,
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
    /// Hints de pie de página de los overlays de diálogo (H1 T3, #24),
    /// PRECOMPUTADOS del efectivo `dialog` vigente — igual que `help_lines`
    /// en `main.rs`, se reconstruyen en el arranque y en cada hot-reload OK
    /// (`main::build_keymaps` + `DialogHints::build`), ANTES de que el
    /// efectivo se mueva al `Resolver` compartido. `ui::draw_*` los lee en
    /// vez de una clave Fluent estática.
    pub dialog_hints: crate::hints::DialogHints,
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
            menu: None,
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
            connections_picker: None,
            columns_picker: None,
            extensions: None,
            lua_pending_trust: None,
            lua_status: None,
            degraded: std::collections::VecDeque::new(),
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
            pending_dest_check: None,
            sync: None,
            pending_sync: None,
            pending_sync_apply: None,
            pending_disconnect_home: None,
            pending_sync_encoding: (None, None),
            // Fail-CLOSED: el `App` de un test no tiene backend, y ofrecer
            // sincronizar por defecto convertiría cada test en un permiso.
            // `main` lo enciende cuando el backend es remoto.
            backend_journalled: false,
            openers: norte_frontend::openers::OpenersConfig::empty(),
            pending_open: None,
            pending_shell: None,
            dialog_hints: crate::hints::DialogHints::default(),
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

/// ALLOWLIST de `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`/
/// `Modal::ConfirmQuit` (S2, `[ui] confirm_quit`): `approve` y `confirm`
/// ambos aceptan (Enter e `y` funcionan igual que antes de H1), `deny`/
/// `cancel` rechazan. Excluye deliberadamente los comandos de
/// colisión/aprobación — un rebind de `w`→`dialog.newer` no hace nada aquí.
pub const ALLOW_CONFIRM: &[&str] = &[
    "dialog.approve",
    "dialog.confirm",
    "dialog.deny",
    "dialog.cancel",
];

/// ALLOWLIST de `Modal::Collision`: overwrite/skip/rename/newer eligen
/// política y reintentan; `cancel` cierra. Excluye A PROPÓSITO
/// `dialog.confirm`/`dialog.approve` — no hay respuesta inocua que Enter
/// deba disparar sola (decisión 4 del plan H1, igual que antes de H1).
pub const ALLOW_COLLISION: &[&str] = &[
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.cancel",
];

/// ALLOWLIST de `Modal::ApproveAgentOp`: SOLO `approve` confirma; `deny` y
/// `cancel` deniegan (cerrar ES denegar, fail-safe). Excluye A PROPÓSITO
/// `dialog.confirm` — aprobar una mutación de AGENTE no es una respuesta
/// inocua que Enter deba disparar sola (decisión 2 del plan H1).
pub const ALLOW_APPROVAL: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST de `Modal::TrustHostKey` (TOFU SSH, #45): mismo principio que
/// [`ALLOW_APPROVAL`] — SOLO `approve` confía, `dialog.confirm` excluido a
/// propósito (Enter jamás confía en una host key sin verificar).
pub const ALLOW_TRUST_HOST: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST del selector de tema (`on_theme_picker_key`, main.rs): sin
/// riesgo de seguridad (elegir tema no muta nada fuera del propio popup),
/// así que `confirm` SÍ dispara (a diferencia de los modales de arriba).
/// Única lista de este overlay — dispatch (main.rs) y el hint generado
/// (H1 T3, `hints::DialogHints`) la comparten, jamás una copia.
pub const ALLOW_PICKER: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del picker de columnas (#108 7a, `on_columns_key`, main.rs) —
/// única fuente para dispatch y para el hint generado del pie
/// (`hints::DialogHints::columns`), patrón #24. `confirm` SÍ aplica+persiste
/// (mismo criterio que [`ALLOW_PICKER`]: elegir columnas solo toca la config
/// propia, no es una mutación de datos que Enter deba proteger).
pub const ALLOW_COLUMNS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del gestor de extensiones (`on_extensions_key`, main.rs,
/// M4-P3): `approve` togglea la aprobación del plugin (decisión 3 del plan
/// H1 — "aprobar un plugin" reutiliza `dialog.approve`), `toggle-enabled`
/// lo activa/desactiva. `confirm` (G3c) abre la sección de `[config]` del
/// plugin resaltado, SI declara alguna clave — Enter jamás aprueba (pin del
/// P1), solo entra en un submenú. Compartida por dispatch y el hint
/// generado.
pub const ALLOW_EXTENSIONS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.approve",
    "dialog.toggle-enabled",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del panel de `[config]` de un plugin (G3c, `on_extensions_key`
/// cuando `mgr.config.is_some()` y NO se está editando un buffer — mientras
/// se edita, las teclas se capturan RAW, mismo criterio que
/// `on_nav_popup_key`'s `name_input`): `up`/`down` mueven el cursor sobre
/// las claves, `confirm` cicla `bool`/`enum` o abre edición de
/// `string`/`int`, `cancel` cierra el panel (vuelve a la lista de plugins,
/// NO cierra el overlay entero).
pub const ALLOW_PLUGIN_CONFIG: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST del sidebar de sitios (L3, `on_places_key` en main.rs).
///
/// El mismo vocabulario `dialog.*` que ya atan los siete presets: un panel que
/// se mueve con flechas y confirma con Enter no necesita idioma propio, y
/// dárselo habría sido siete presets tocados por una tecla nueva.
/// `toggle-enabled` pliega la sección, `cancel` devuelve el teclado a los
/// listados SIN cerrar el sidebar — cerrarlo es cosa de `layout.places`.
pub const ALLOW_PLACES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
    // Ancho: con el teclado DENTRO, `layout.grow`/`shrink` cambian el ancho
    // de ESTE panel. Es el único camino por el que se puede — el llamante de
    // `layout_resize` pasa siempre un listado visible (#244 M1).
    "layout.grow",
    "layout.shrink",
    // Su PROPIA tecla, que por eso está atada en `[global]`: sin ella el
    // sidebar se queda el `alt+b` y no puede cerrarse a sí mismo — abrías el
    // panel y la misma tecla dejaba de existir. Lo destapó pilotar la TUI en
    // tmux con la suite entera en verde, que es exactamente para lo que
    // sirve el harness.
    "layout.places",
];

/// ALLOWLIST del panel de procesos (`on_processes_key` en main.rs).
///
/// El mismo vocabulario `dialog.*` del sidebar, por lo mismo: un panel que se
/// mueve con flechas y actúa con Enter no necesita idioma propio, y dárselo
/// serían siete presets tocados por una tecla nueva. `confirm` CANCELA la
/// tarea bajo el cursor —es la única acción que el protocolo tiene sobre una
/// task—, `cancel` devuelve el teclado a los listados sin cerrar el panel, y
/// `layout.processes` cierra desde dentro (tercera pulsación de abrir →
/// enfocar → cerrar, igual que `layout.places`).
pub const ALLOW_PROCESSES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
    "layout.grow",
    "layout.shrink",
    "layout.processes",
];

/// ALLOWLIST de DESPACHO del popup de navegación (`on_nav_popup_key`,
/// main.rs), unión de lo que History, Hotlist y Volumes aceptan: `add`/
/// `remove` los filtra el caller a `kind == Hotlist` (nada que nombrar ni
/// borrar en historial o volúmenes) y `toggle-enabled` a `kind == Volumes`
/// (el toggle "mostrar todo" no significa nada en los otros dos) — mismo
/// criterio que antes de H1.
///
/// El HINT impreso es más estrecho que esto por kind: [`ALLOW_NAV_HOTLIST`]
/// y [`ALLOW_NAV_VOLUMES`] son los que de verdad pinta cada footer (design
/// §D — el footer de volúmenes no debe ofrecer "añadir"/"borrar", que no
/// significan nada sobre un volumen montado).
pub const ALLOW_NAV_POPUP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.toggle-enabled",
    "dialog.cancel",
];

/// HINT del popup en modo HOTLIST (H1 T3): historial y volúmenes pintan el
/// suyo propio (o ninguno) — ver [`ALLOW_NAV_POPUP`] para el porqué de la
/// separación.
pub const ALLOW_NAV_HOTLIST: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.cancel",
];

/// HINT del popup en modo VOLUMES (design §D): navegación, confirmar,
/// cancelar y el toggle "mostrar todo" — nada de `add`/`remove`.
pub const ALLOW_NAV_VOLUMES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
];

/// Verbs the help overlay dispatches (H3b). Navigation, confirm (run the
/// focused row or follow the focused link), cancel (close), plus its own
/// three. Nothing that mutates: the overlay itself changes no files — a
/// command it RUNS goes through the normal dispatch, with its own
/// confirmation, gate and journal entry.
pub const ALLOW_HELP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.confirm",
    "dialog.cancel",
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
];

/// Mapea un comando `dialog.*` YA RESUELTO (por el
/// [`Resolver`](crate::keymap::Resolver) del efectivo `dialog`, H1 #24) al
/// desenlace del modal activo, filtrando por el ALLOWLIST del modal
/// concreto: `None` = comando fuera de allowlist, la tecla es INERTE para
/// este modal (p. ej. Enter — `dialog.confirm` — sobre una aprobación de
/// agente). La semántica de seguridad vive aquí, en código, jamás en el
/// keymap: un rebind solo cambia qué TECLA dispara `dialog.approve`, nunca
/// qué modales aceptan `dialog.approve` como confirmación.
///
/// `Modal::TrustLuaInit` no tiene allowlist — decisión 8 del plan H1, se
/// resuelve aparte con [`trust_lua_key`] — y devuelve `None` aquí siempre.
#[must_use]
pub fn dialog_action(modal: &Modal, cmd: &str) -> Option<DialogOutcome> {
    use norte_proto::CollisionPolicy as P;
    match modal {
        // #139: las propiedades no PREGUNTAN nada — se leen y se cierran—, así
        // que solo entienden cancelar. Darle un «confirmar» a un cuadro de
        // solo lectura es enseñarle al lector que Enter hace algo aquí.
        Modal::Properties { .. } => (cmd == "dialog.cancel").then_some(DialogOutcome::Cancelled),
        // M4-IA: `AiRenamePlan` es una superficie de decisión sobre contenido
        // INICIADO y REVISADO por el humano — semántica [`ALLOW_CONFIRM`]
        // (Enter confirma, como un delete/transfer), NO el allowlist de
        // aprobación de agentes (`ALLOW_APPROVAL`, que excluye confirm).
        //
        // Con una salvedad que este brazo aparte existe para imponer (spec
        // §17): confirmar necesita un plan de lote APLICABLE. Sin plan no hay
        // `plan_hash` aprobado que mandar, y con veredictos el core no
        // ejecutaría nada — en ambos casos la tecla de confirmar queda MUDA
        // (cancelar sigue vivo), y el pie del modal deja de ofrecerla
        // (`modal-rename-batch-plan-hint-blocked`). La decisión de si un plan
        // se puede ejecutar es del core: aquí solo se lee `executable`.
        Modal::AiRenamePlan { plan, .. } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            let confirms = matches!(cmd, "dialog.approve" | "dialog.confirm");
            if confirms && !plan.confirmable() {
                return None;
            }
            Some(if confirms {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled // dialog.deny | dialog.cancel
            })
        }
        // M4-IA-2: `SemanticHits` es igualmente una superficie de decisión
        // sobre contenido PEDIDO por el humano — Enter navega al hit bajo el
        // cursor, no muta nada.
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmTransfer { .. }
        | Modal::ConfirmQuit
        | Modal::SemanticHits { .. } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" | "dialog.confirm" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::Collision { .. } => {
            if !ALLOW_COLLISION.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.overwrite" => DialogOutcome::Retry(P::Overwrite),
                "dialog.skip" => DialogOutcome::Retry(P::Skip),
                "dialog.rename" => DialogOutcome::Retry(P::RenameAuto),
                "dialog.newer" => DialogOutcome::Retry(P::Newer),
                _ => DialogOutcome::Cancelled, // dialog.cancel
            })
        }
        Modal::ApproveAgentOp { .. } => {
            if !ALLOW_APPROVAL.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::TrustHostKey { .. } => {
            if !ALLOW_TRUST_HOST.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        // #103 T9: `MarkPattern` es texto libre, como el diálogo de
        // búsqueda — el run loop lo intercepta ANTES de llegar aquí (raw
        // chars, jamás el contexto `dialog`), igual que `TrustLuaInit`.
        // Ambos devuelven `None` siempre.
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferDest { .. }
        // #132: los dos de escribir archivos, por lo mismo.
        | Modal::Pack { .. }
        | Modal::Split { .. }
        | Modal::TransferName { .. } => None,
    }
}

/// What a `dialog.*` verb does inside the help overlay (H3b).
///
/// A vocabulary of its own rather than [`DialogOutcome`]: a modal answers a
/// QUESTION (confirm/deny/retry-with-a-policy) and this overlay is a reader —
/// its verbs move a cursor, follow a link and close a window. Sharing the
/// enum would force every modal to carry arms it can never produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    /// One row up: a topic in the sidebar, an action in the body.
    Up,
    /// One row down.
    Down,
    /// A page up: topics in the sidebar, body lines in the body.
    PageUp,
    /// A page down.
    PageDown,
    /// Enter: open the selected topic, or run/follow the focused body row.
    Activate,
    /// Close the overlay.
    Close,
    /// Hand the focus to the other half.
    TogglePane,
    /// Back to the previously open topic.
    Back,
    /// Start typing into the sidebar filter.
    StartFilter,
}

/// Maps an already-resolved `dialog.*` command to what it means inside the
/// help overlay, or `None` for a verb the overlay does not support — the key
/// is INERT, exactly as in [`dialog_action`].
///
/// Filtered through the SAME [`ALLOW_HELP`] the footer hint is generated from
/// ([`crate::hints::DialogHints::help`]), never a second copy: a verb the
/// footer advertises and dispatch drops (or the reverse) is a hint that lies,
/// and one list cannot drift from itself.
#[must_use]
pub fn help_action(cmd: &str) -> Option<HelpOutcome> {
    if !ALLOW_HELP.contains(&cmd) {
        return None;
    }
    Some(match cmd {
        "dialog.up" => HelpOutcome::Up,
        "dialog.down" => HelpOutcome::Down,
        "dialog.page-up" => HelpOutcome::PageUp,
        "dialog.page-down" => HelpOutcome::PageDown,
        "dialog.confirm" => HelpOutcome::Activate,
        "dialog.cancel" => HelpOutcome::Close,
        "dialog.pane" => HelpOutcome::TogglePane,
        "dialog.back" => HelpOutcome::Back,
        "dialog.filter" => HelpOutcome::StartFilter,
        // Unreachable through the allowlist above, and deliberately not an
        // `unreachable!`: a verb added to `ALLOW_HELP` without an arm here is
        // an inert key, never a panic in a reader's terminal. The test
        // `help_action_accepts_exactly_the_allowlist` is what catches it.
        _ => return None,
    })
}

/// Resuelve el modal [`Modal::TrustLuaInit`] (decisión 8 del plan H1: NO
/// migrado al contexto `dialog` — es una ruta de resolución ESPECIAL que el
/// run loop intercepta ANTES de consultar el keymap, porque necesita el
/// `LuaHost` que solo vive ahí). Mismo contrato de seguridad que el resto de
/// diálogos TOFU: `y` confía, `n`/Esc deniegan, Enter NO decide (sin default
/// peligroso que se dispare solo).
#[must_use]
pub fn trust_lua_key(code: crossterm::event::KeyCode) -> DialogOutcome {
    use crossterm::event::KeyCode as K;
    match code {
        K::Char('y') => DialogOutcome::Confirmed,
        K::Char('n') | K::Esc => DialogOutcome::Cancelled,
        _ => DialogOutcome::Open,
    }
}

/// El vocabulario de CATEGORÍAS de error vive en [`norte_frontend::error`]
/// (#158, revisión de la fase C1): la GUI dice los mismos errores y no podía
/// alcanzarlo aquí, así que interpolaba el `Display` inglés en frases por lo
/// demás localizadas. Se re-exporta con el nombre de siempre porque es API
/// pública de este crate (los scripts Lua comparan contra estas claves).
pub use norte_frontend::error::{error_category, error_key};

/// Mensaje de barra `error: <categoría>` (envuelve [`error_category`]).
#[must_use]
pub fn error_message(e: &Error) -> String {
    ta("msg-error", &[("error", &error_category(e))])
}

/// Tope del detalle diagnóstico en la barra (una línea; un TOML hostil puede
/// citar valores kilométricos).
pub const DETAIL_MAX_CHARS: usize = 160;

/// Detalle diagnóstico listo para la barra (#73): enmascarado como un nombre
/// ([`display_name`]: lossy marcado, sin controles/bidi/invisibles) y con
/// tope [`DETAIL_MAX_CHARS`] (recorte marcado con `…`).
#[must_use]
pub fn detail_for_bar(detail: &str) -> String {
    let (masked, _) = display_name(detail.as_bytes());
    let mut out: String = masked.chars().take(DETAIL_MAX_CHARS).collect();
    if masked.chars().nth(DETAIL_MAX_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// Categoría LOCALIZADA de un error de io LOCAL (#73): `ErrorKind` → clave
/// Fluent — jamás el `Display` del OS, que el SO localiza a su antojo
/// («Permission denied (os error 13)»; regla 1).
#[must_use]
pub fn io_error_category(e: &std::io::Error) -> String {
    let key = match e.kind() {
        std::io::ErrorKind::NotFound => "err-not-found",
        std::io::ErrorKind::PermissionDenied => "err-permission-denied",
        std::io::ErrorKind::StorageFull => "err-no-space",
        _ => "err-io",
    };
    t(key)
}

/// Categoría LOCALIZADA de un [`crate::config::ConfigError`] (#73): path
/// propio (lossy explícito + mask) y, en el caso TOML, el diagnóstico del
/// parser saneado por [`detail_for_bar`] — la posición («at line N») es lo
/// accionable. El io subyacente va por [`io_error_category`].
#[must_use]
pub fn config_error_category(e: &crate::config::ConfigError) -> String {
    use crate::config::ConfigError;
    match e {
        ConfigError::Io { path, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("error", &io_error_category(source)),
            ],
        ),
        ConfigError::Toml { path, message } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(&path.display().to_string())),
                ("detail", &detail_for_bar(message)),
            ],
        ),
    }
}

/// Categoría LOCALIZADA de un [`crate::theme::ResolveError`] (#73), espejo
/// de [`config_error_category`]. El `spec` puede venir de la capa `./.norte`
/// de un repo AJENO: siempre por [`detail_for_bar`].
#[must_use]
pub fn theme_error_category(e: &crate::theme::ResolveError) -> String {
    use crate::theme::ResolveError;
    match e {
        ResolveError::Io { spec, source } => ta(
            "err-config-io",
            &[
                ("path", &detail_for_bar(spec)),
                ("error", &io_error_category(source)),
            ],
        ),
        ResolveError::Parse { spec, detail } => ta(
            "err-config-parse",
            &[
                ("path", &detail_for_bar(spec)),
                ("detail", &detail_for_bar(detail)),
            ],
        ),
    }
}

/// Error tipado del montaje de keymaps (#73): cada variante mapea a una
/// clave Fluent en [`keymaps_error_category`] — nada de contextos anyhow
/// castellanos hardcodeados en la barra. El `Display` (thiserror) solo sale
/// por stderr en el arranque, antes de levantar la TUI.
#[derive(Debug, thiserror::Error)]
pub enum KeymapsError {
    /// El preset pedido (CLI o config) no existe.
    #[error("preset desconocido {name:?}; disponibles: {available}")]
    UnknownPreset {
        /// Lo pedido.
        name: String,
        /// Los que sí existen, ya unidos para display.
        available: String,
    },
    /// Una capa de keymap no valida contra los comandos.
    #[error("keymap inválido: {detail}")]
    Invalid {
        /// Diagnóstico del validador ([`crate::keymap::KeymapError`]).
        detail: String,
    },
}

/// Categoría LOCALIZADA de un [`KeymapsError`] (#73).
#[must_use]
pub fn keymaps_error_category(e: &KeymapsError) -> String {
    match e {
        KeymapsError::UnknownPreset { name, available } => ta(
            "err-keymap-preset-unknown",
            &[("name", &detail_for_bar(name)), ("available", available)],
        ),
        KeymapsError::Invalid { detail } => {
            ta("err-keymap-invalid", &[("detail", &detail_for_bar(detail))])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Entry;
    use norte_proto::{EntryKind, Scheme};

    fn root() -> VPath {
        VPath::root(Scheme::new("mem").unwrap(), None)
    }

    fn file(name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: root().join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    fn names(p: &Pane) -> Vec<String> {
        p.entries()
            .iter()
            .map(|e| String::from_utf8_lossy(e.path.file_name().unwrap().as_bytes()).into_owned())
            .collect()
    }

    /// `extend_listing` re-ordena TODO el listado (primera página + lote).
    #[test]
    fn extend_reordena_todo() {
        let mut first = vec![file("b.txt"), file("d.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_loading(true);
        p.extend_listing(vec![file("a.txt"), file("c.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "c.txt", "d.txt"]);
    }

    /// El cursor se re-ancla al PATH seleccionado, no al índice: rellenar no
    /// mueve la selección del usuario bajo sus pies.
    #[test]
    fn extend_reancla_el_cursor_por_path() {
        let mut first = vec![file("m.txt"), file("z.txt")];
        sort_entries(&mut first);
        let mut p = Pane::new(root(), first);
        p.set_cursor(1); // "z.txt"
        // Llega un lote de nombres que ordenan ANTES: z.txt se desplaza.
        p.extend_listing(vec![file("a.txt"), file("b.txt")]);
        assert_eq!(names(&p), vec!["a.txt", "b.txt", "m.txt", "z.txt"]);
        assert_eq!(
            p.selected().unwrap().path.file_name().unwrap().as_bytes(),
            b"z.txt"
        );
    }

    /// Un lote vacío no altera nada (fin del drenado sin cola).
    #[test]
    fn extend_vacio_es_noop() {
        let mut p = Pane::new(root(), vec![file("a.txt")]);
        p.set_cursor(0);
        p.extend_listing(vec![]);
        assert_eq!(names(&p), vec!["a.txt"]);
        assert_eq!(p.cursor(), 0);
    }

    /// `finish_listing` limpia el flag de carga.
    #[test]
    fn finish_limpia_loading() {
        let mut p = Pane::new(root(), vec![]);
        p.set_loading(true);
        p.finish_listing();
        assert!(!p.loading());
    }

    /// #52: `needs_stat_window` hidrata lo VISIBLE, no solo lo enfocado —
    /// las columnas Tamaño/Fecha salían en blanco en todas las filas salvo
    /// la del cursor. Los dos panes se pintan a la vez, así que los dos
    /// aportan candidatas (el enfocado primero); fuera del radio, no; un
    /// Dir, nunca; ya hidratada, tampoco.
    #[test]
    fn needs_stat_window_cubre_los_dos_panes_dentro_del_radio() {
        let lazy = |n: &str| {
            let mut e = file(n);
            e.size = None;
            e
        };
        let mut dir_lazy = lazy("z-dir");
        dir_lazy.kind = EntryKind::Dir;
        let left = vec![lazy("a.txt"), lazy("b.txt"), lazy("c.txt"), dir_lazy];
        let right = vec![lazy("d.txt"), file("e.txt")];
        let mut app = App::new(Pane::new(root(), left), Pane::new(root(), right));
        // `Pane::new` ordena (dirs primero): [z-dir, a, b, c].
        app.panes[0].set_cursor(1);

        let window = app.needs_stat_window(1);
        let names: Vec<String> = window
            .iter()
            .map(|(p, path)| format!("{p}:{}", path.display_lossy()))
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("0:") && n.ends_with("/a.txt"))
                && names
                    .iter()
                    .any(|n| n.starts_with("0:") && n.ends_with("/b.txt")),
            "cursor ± radio del pane con foco: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("c.txt")),
            "fuera del radio no se sondea: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("z-dir")),
            "un Dir jamás se sondea: {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("1:") && n.ends_with("/d.txt")),
            "el pane SIN foco también se pinta: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("e.txt")),
            "ya hidratada, no es candidata: {names:?}"
        );
        assert_eq!(window[0].0, 0, "el pane con foco va primero");

        // Un radio generoso alcanza el listado entero de ambos panes.
        assert_eq!(app.needs_stat_window(64).len(), 4);
    }

    /// #52: `focused_needs_stat` señala la entrada File enfocada SIN `size`
    /// (candidata a la sonda lazy). Ya hidratada o siendo un Dir, no aplica.
    #[test]
    fn focused_needs_stat_solo_file_lazy() {
        let mut lazy = file("a.txt");
        lazy.size = None;
        let mut app = App::new(
            Pane::new(root(), vec![lazy.clone()]),
            Pane::new(root(), vec![]),
        );
        assert_eq!(
            app.focused_needs_stat(),
            Some((0, lazy.path.clone())),
            "File sin size es candidato"
        );

        // Ya hidratada: deja de ser candidata.
        app.panes[0].hydrate(&lazy.path, Some(5), None);
        assert!(app.focused_needs_stat().is_none(), "ya tiene size");

        // Un Dir jamás se sondea, aunque venga sin size.
        let mut dir_lazy = file("b");
        dir_lazy.kind = EntryKind::Dir;
        dir_lazy.size = None;
        app.panes[0] = Pane::new(root(), vec![dir_lazy]);
        assert!(app.focused_needs_stat().is_none(), "un Dir no se sondea");
    }

    /// Fila de comparación de un lado (huérfano) con la clase y el tamaño
    /// pedidos, para las pruebas de `compare_size_probe_targets` (#157).
    fn fila_huerfana(
        id: u64,
        kind: EntryKind,
        size: Option<u64>,
    ) -> norte_proto::methods::CompareRow {
        use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};
        norte_proto::methods::CompareRow {
            id,
            left: Some(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: root()
                    .join(norte_proto::Segment::new(format!("f{id}").into_bytes()).unwrap()),
                kind,
                size,
                mtime_ms: None,
            }),
            right: None,
            verdict: CompareVerdict::OnlyLeft,
            criterion: CompareCriterion::Presence,
            confidence: CompareConfidence::Certain,
            newer: None,
            reason: None,
            side: None,
            paired_under: None,
        }
    }

    /// #157: un huérfano `File` sin `size` es candidato a la sonda de la fila
    /// seleccionada, y deja de serlo en cuanto `hydrate_compare_size` lo
    /// resuelve — con éxito o sin él, para no reintentarlo cada frame.
    #[test]
    fn compare_size_probe_targets_solo_file_sin_size_y_no_repite() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = fila_huerfana(1, EntryKind::File, None);
        let path = row.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![row]);
        app.compare = Some(view);

        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "huérfano File sin size es candidato"
        );

        // Sondeado con ÉXITO: ya no es candidato, y el hint queda puesto.
        app.hydrate_compare_size(app.compare_generation(), path.clone(), Some(42));
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "ya hidratado, no se repite"
        );
        assert_eq!(app.compare_size_hints.get(&path), Some(&42));
    }

    /// #198: una sonda lanzada para la comparación A no puede aterrizar en
    /// la B. La sonda vive en el run loop y `launch_compare` no la ve, así
    /// que la única defensa es que el resultado traiga la generación bajo la
    /// que se pidió — sin eso, la caché que el rustdoc llama «de ESTA
    /// comparación» tiene dentro un tamaño de la anterior, en el panel cuyo
    /// asunto entero es si lo que estás mirando es exacto.
    #[test]
    fn una_sonda_de_la_comparacion_anterior_no_aterriza_en_la_nueva() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = fila_huerfana(1, EntryKind::File, None);
        let path = row.left.as_ref().expect("izquierda").path.clone();
        view.pane.extend(vec![row.clone()]);
        app.compare = Some(view);
        let old = app.compare_generation();

        // Otra comparación empieza: la caché se vacía y la generación avanza.
        app.begin_compare_generation();
        let mut view = CompareView::new(vp("mem:///c"), vp("mem:///d"), 0, None, None);
        view.pane.extend(vec![row]);
        app.compare = Some(view);
        assert_ne!(app.compare_generation(), old);

        // Llega la sonda de la comparación VIEJA.
        app.hydrate_compare_size(old, path.clone(), Some(42));
        assert!(
            app.compare_size_hints.is_empty(),
            "ni el tamaño de la anterior"
        );
        assert_eq!(
            app.compare_size_probe_targets(),
            vec![path.clone()],
            "ni marcado sondeado: la nueva todavía tiene que pedirlo"
        );

        // Y la de la nueva sí.
        let now = app.compare_generation();
        app.hydrate_compare_size(now, path.clone(), Some(7));
        assert_eq!(app.compare_size_hints.get(&path), Some(&7));
    }

    /// Un `stat` que falla (`None`) también se marca sondeado: no se
    /// reintenta cada frame contra un provider roto, mismo criterio que
    /// `last_probed` en el pane normal.
    #[test]
    fn compare_size_probe_targets_no_reintenta_un_stat_fallido() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        let row = fila_huerfana(1, EntryKind::File, None);
        let path = row.left.as_ref().unwrap().path.clone();
        view.pane.extend(vec![row]);
        app.compare = Some(view);

        app.hydrate_compare_size(app.compare_generation(), path, None);
        assert!(
            app.compare_size_probe_targets().is_empty(),
            "un fallo también se marca sondeado"
        );
        assert!(app.compare_size_hints.is_empty(), "sin hint sobre un fallo");
    }

    /// Un directorio o un huérfano que YA trae `size` no son candidatos —
    /// mismo criterio que `focused_needs_stat` para el pane normal: un `Dir`
    /// no tiene un tamaño que un `stat` corriente resuelva.
    #[test]
    fn compare_size_probe_targets_ignora_dir_y_lo_ya_hidratado() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        let mut view = CompareView::new(vp("mem:///a"), vp("mem:///b"), 0, None, None);
        view.pane.extend(vec![
            fila_huerfana(1, EntryKind::Dir, None),
            fila_huerfana(2, EntryKind::File, Some(7)),
        ]);
        app.compare = Some(view);

        assert!(
            app.compare_size_probe_targets().is_empty(),
            "un Dir sin size y un File que ya lo trae no son candidatos"
        );
    }

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Pane sobre `mem://` con archivos nombrados como se pida: #54, `Pane`
    /// (vía `PaneState::new`) normaliza el orden internamente (dirs primero,
    /// NFC, empate por bytes) — los tests del quick search razonan sobre el
    /// índice real YA ORDENADO, no sobre el orden de llegada de `names`.
    fn pane_con(names: &[&str]) -> Pane {
        Pane::new(root(), names.iter().map(|n| file(n)).collect())
    }

    /// Filtro activo: `selected()` (la base de F5/F8/F3…) apunta a la
    /// selección DENTRO del filtro; cancelar restaura el listado completo
    /// con el cursor real donde estaba (el filtro jamás lo movió).
    #[test]
    fn quick_filter_redirige_seleccion_y_ops() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "selected respeta el filtro"
        );
        p.quick_down();
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        p.quick_cancel();
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a1"),
            "restaurado: cursor al último real"
        );
    }

    /// Confirmar fija el cursor REAL a lo seleccionado en el filtro y cierra
    /// (Enter: la op siguiente —cd, view— parte de ese cursor).
    #[test]
    fn quick_confirm_fija_el_cursor_real() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down();
        p.quick_confirm();
        assert!(p.quick().is_none(), "confirmar cierra el quick search");
        // #54: normalizado, el orden real es [a1, a2, b] — a2 al índice 1.
        assert_eq!(p.cursor(), 1, "cursor real = índice real de a2");
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
    }

    /// Un lote nuevo del fill re-aplica el filtro (spec: al llegar lotes
    /// nuevos el filtro se re-aplica, no se congela).
    #[test]
    fn extend_listing_reaplica_el_filtro() {
        let mut p = pane_con(&["a1"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.extend_listing(vec![file("a2"), file("zz")]);
        assert_eq!(p.quick_visible().unwrap().len(), 2, "a2 entra, zz no");
    }

    /// review MAJOR T4: con el filtro SIN matches la pantalla lista vacío —
    /// Enter jamás debe actuar sobre la entrada del cursor real (invisible
    /// para el usuario). `quick_confirm` devuelve false y no toca el cursor.
    #[test]
    fn enter_sin_matches_no_actua_sobre_entrada_invisible() {
        let mut p = pane_con(&["a1", "b", "a2"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('x'); // cero matches
        assert!(p.selected().is_none(), "sin matches no hay selección");
        assert!(
            !p.quick_confirm(),
            "confirmar sin matches NO fija selección"
        );
        assert!(p.quick().is_none(), "el quick search sí se cierra");
        assert_eq!(p.cursor(), 1, "el cursor real queda intacto");
    }

    /// Modo salto: el listado NO cambia; teclear mueve el cursor REAL al
    /// primer match y Tab (`quick_next`) al siguiente con wrap.
    #[test]
    fn quick_jump_mueve_el_cursor_real() {
        // #54: normalizado, el orden real es [ab, ac, zz] — ab y ac casan.
        let mut p = pane_con(&["ab", "zz", "ac"]);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('a');
        assert_eq!(p.cursor(), 0, "salta al primer match");
        assert!(
            p.quick_visible().is_none(),
            "en salto el listado queda intacto"
        );
        p.quick_next();
        assert_eq!(p.cursor(), 1, "Tab: siguiente match");
        p.quick_next();
        assert_eq!(p.cursor(), 0, "wrap");
        assert_eq!(p.selected().unwrap().path, vp("mem:///ab"));
    }

    /// El contrato de `QuickSearch::refresh` (T1) de punta a punta:
    /// `extend_listing` RE-SORTEA el listado entero, así que la selección
    /// del filtro se conserva por PATH, jamás por índice.
    #[test]
    fn extend_con_resort_conserva_seleccion_por_path() {
        let mut p = pane_con(&["a1", "a2"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a');
        p.quick_down(); // selecciona a2 (índice real 1)
        assert_eq!(p.selected().unwrap().path, vp("mem:///a2"));
        // "a0" ordena ANTES: a2 pasa del índice real 1 al 2 tras el sort.
        p.extend_listing(vec![file("a0")]);
        assert_eq!(
            p.selected().unwrap().path,
            vp("mem:///a2"),
            "la selección sigue en el MISMO path tras el resort"
        );
    }

    /// Edge de T4 (review): en Jump con query SIN matches el listado se
    /// pinta ENTERO — el cursor real es visible por definición, así que
    /// Enter SÍ puede operar sobre él (en Filter sigue siendo `false`).
    #[test]
    fn enter_en_jump_sin_matches_opera_sobre_el_cursor_visible() {
        let mut p = pane_con(&["a1", "b"]);
        p.set_cursor(1);
        p.quick_start(crate::nav::Mode::Jump);
        p.quick_char('x'); // cero matches; el listado no cambió
        assert!(
            p.quick_confirm(),
            "en Jump el cursor real ES visible: Enter opera"
        );
        assert!(p.quick().is_none(), "el quick search se cierra");
        assert_eq!(p.cursor(), 1, "el cursor real queda donde estaba");

        // Con el pane VACÍO ni Jump confirma (no hay nada visible).
        let mut empty = pane_con(&[]);
        empty.quick_start(crate::nav::Mode::Jump);
        assert!(
            !empty.quick_confirm(),
            "sin entradas no hay nada que operar"
        );
    }

    fn app_dos_panes() -> App {
        App::new(pane_con(&["a"]), pane_con(&["b"]))
    }

    /// Unas caps cualesquiera: lo que se prueba es el CACHÉ por scheme, no
    /// qué flags trae el provider.
    fn caps_de_test() -> norte_proto::Capabilities {
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::RENAME_ATOMIC,
            max_path: None,
        }
    }

    /// Las caps se cachean por LOCALIZACIÓN, y por la misma razón que el
    /// catálogo de atributos se pide: `fs.capabilities` devuelve las dos
    /// mitades en UNA llamada y la TUI ya la hace para las columnas. Tirar la
    /// mitad de caps y luego sondear otra vez sería pagar dos rondas por un
    /// dato que ya llegó.
    #[test]
    fn las_caps_se_cachean_por_localizacion() {
        let mut app = app_dos_panes();
        let mem = vp("mem:///");
        assert!(app.caps(&mem).is_none(), "sin sembrar, no se inventa nada");
        app.insert_caps(&mem, caps_de_test());
        assert!(app.caps(&mem).is_some());
        assert!(
            app.caps(&vp("sftp://ejemplo.org/")).is_none(),
            "un scheme no responde por otro"
        );
    }

    /// MAJOR-1: `sftp` no es UN sitio. Dos hosts del mismo scheme son dos
    /// backends distintos, y el caché tiene que contarlos aparte o el primero
    /// que contesta decide por todos los demás durante la sesión entera. Hoy
    /// ningún provider del árbol declara `READ_ONLY` por localización (el
    /// archivo y los plugins lo deciden por scheme), así que la clave por
    /// scheme sola no fallaba — por suerte, no por diseño, y `App::caps` es un
    /// accesor general que invita a leer cualquier flag.
    #[test]
    fn dos_authorities_del_mismo_scheme_no_se_responden() {
        let a = vp("sftp://a.org/");
        let b = vp("sftp://b.org/");
        let mut app = App::new(
            Pane::new(a.clone(), Vec::new()),
            Pane::new(b.clone(), Vec::new()),
        );
        app.insert_caps(
            &a,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0), "a.org dijo que es de solo lectura");
        assert!(
            app.caps(&b).is_none(),
            "a b.org no se le ha preguntado nada todavía"
        );
        assert!(
            !app.pane_read_only(1),
            "b.org no puede heredar el veto de a.org: son dos backends"
        );
    }

    /// Antes de que llegue la primera respuesta, la respuesta honesta es «no
    /// lo sé», y quien pregunta cae al criterio SINTÁCTICO (el scheme dice si
    /// es un archivo comprimido). Lo que no puede hacer es afirmar que se
    /// puede escribir.
    #[test]
    fn sin_caps_todavia_el_solo_lectura_lo_decide_el_scheme() {
        let app = app_dos_panes();
        assert!(!app.pane_read_only(0), "mem:// no es de solo lectura");

        let inside_a_zip = app_en("zip+file:///a.zip/!", "file:///casa");
        assert!(
            inside_a_zip.pane_read_only(0),
            "un scheme de archivo es de solo lectura por construcción"
        );
        assert!(!inside_a_zip.pane_read_only(1));
    }

    /// Cuando las caps SÍ llegaron mandan ellas: un provider que anuncia
    /// `READ_ONLY` sobre un scheme que sintácticamente no lo es (un montaje
    /// remoto en solo lectura) se veta igual.
    #[test]
    fn con_caps_manda_el_flag_read_only() {
        let mut app = app_dos_panes();
        let dir = app.panes[0].dir().clone();
        app.insert_caps(
            &dir,
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
        );
        assert!(app.pane_read_only(0));
        app.insert_caps(&dir, caps_de_test());
        assert!(!app.pane_read_only(0), "sin el flag, escribible");
    }

    /// Los hechos que la ayuda congela salen de los MISMOS predicados que usan
    /// los brazos de `dispatch`: un `.zip` se ENTRA en la TUI (`nav.enter`
    /// compone el scheme) aunque sea un File, y `pane.view` quiere File o
    /// Symlink. Derivarlos otra vez aquí sería atenuar filas que la app
    /// ejecutaría.
    #[test]
    fn los_hechos_de_la_ayuda_siguen_a_los_predicados_del_dispatch() {
        let mut app = app_dos_panes();
        // El cursor está sobre un File normal: no se entra, se ve.
        let f = app.help_facts();
        assert!(!f.enterable, "un fichero cualquiera no se entra");
        assert!(f.viewable);
        assert!(f.rename_single, "shift+F6 renombra UNA: la del cursor");
        assert!(!f.source_read_only && !f.dest_read_only);
        assert!(!f.degraded);

        // Un `.zip` ES entrable en la TUI aunque su kind sea File.
        let zip = Pane::new(
            root(),
            vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: root().join(norte_proto::Segment::new(b"a.zip".to_vec()).unwrap()),
                kind: EntryKind::File,
                size: Some(1),
                mtime_ms: None,
            }],
        );
        let app_zip = App::new(zip, pane_con(&["b"]));
        assert!(
            app_zip.help_facts().enterable,
            "en la TUI un .zip se entra: la ayuda no puede decir lo contrario"
        );

        // Y la degradación del scheme del pane con foco llega al hecho.
        app.note_degraded(degradacion_de_test("mem", "sin-host"));
        assert!(app.help_facts().degraded);
    }

    /// Con VARIAS marcas la ayuda NO atenúa shift+F6, porque la TUI lo
    /// ejecuta: `Command::PaneRename` va a `open_rename`, que renombra
    /// `selected()` y no mira las marcas. Atenuarlo sería el fallo exacto que
    /// H3d existe para no cometer — apagar una fila que la app habría corrido,
    /// que enseña al lector a no volver a intentarlo.
    ///
    /// (La GUI sí se niega con selección múltiple, y su menú lo sigue haciendo:
    /// `norte_gui::context_menu`, `renombrar_es_una_sola_entrada_y_la_de_ia_es_otra`.
    /// El hecho es del llamador precisamente porque las dos respuestas son
    /// correctas.)
    #[test]
    fn con_varias_marcas_la_ayuda_no_atenua_renombrar() {
        use norte_help::ChordResolver as _;

        let mut app = App::new(pane_con(&["a", "b", "c"]), pane_con(&["z"]));
        app.focused_mut().toggle_mark_and_advance();
        app.focused_mut().toggle_mark_and_advance();
        assert_eq!(
            app.focused().marked_paths().len(),
            2,
            "hay DOS marcas: el caso que se atenuaba"
        );
        assert!(app.help_facts().rename_single);

        app.freeze_help_facts();
        assert!(
            app.help_chords.availability("pane.rename").is_available(),
            "la TUI renombra la del cursor con marcas puestas: la ayuda no puede negarlo"
        );
        // Y dentro de un archivo sí se apaga, por el backend — el veto real
        // sigue en pie.
        let mut zip = App::new(
            Pane::new(vp("zip+file:///a.zip/!"), Vec::new()),
            pane_con(&["z"]),
        );
        zip.freeze_help_facts();
        assert_eq!(
            zip.help_chords.availability("pane.rename").reason(),
            Some(norte_help::Reason::ReadOnlyBackend)
        );
    }

    /// Congelar los hechos al abrir la ayuda: el resolver que la vista usa
    /// pasa a responder con los hechos de ESE momento.
    #[test]
    fn congelar_los_hechos_reescribe_el_resolver_de_la_ayuda() {
        use norte_help::ChordResolver as _;

        let mut app = app_en("zip+file:///a.zip/!", "zip+file:///b.zip/!");
        assert!(
            app.help_chords.availability("pane.copy").is_available(),
            "antes de congelar el resolver no sabe nada del contexto"
        );
        app.freeze_help_facts();
        assert_eq!(
            app.help_chords.availability("pane.copy").reason(),
            Some(norte_help::Reason::ReadOnlyBackend),
            "los dos panes son de solo lectura: copiar no tiene destino"
        );
    }

    /// Una notif `connection.degraded` como la del wire (#44).
    fn degradacion_de_test(scheme: &str, host: &str) -> norte_proto::methods::ConnectionDegraded {
        norte_proto::methods::ConnectionDegraded {
            scheme: scheme.to_owned(),
            host: host.to_owned(),
            reason: "ftp-plaintext".to_owned(),
            detail: None,
        }
    }

    /// #44 guardaba la degradación como PROSA ya formateada: el scheme y el
    /// host se metían en el mensaje y se tiraban, así que «¿qué conexión se
    /// degradó?» no tenía respuesta. H3d la necesita por pane.
    #[test]
    fn la_degradacion_se_guarda_por_scheme() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ejemplo.org"));
        let d = app.degraded_for("sftp").expect("la degradación se retuvo");
        assert_eq!(d.host, "ejemplo.org", "el host sobrevive, no solo la frase");
        assert_eq!(d.reason, "ftp-plaintext");
        assert!(app.degraded_for("file").is_none());
    }

    /// Y dos conexiones degradadas no se pisan: antes la última ganaba y la
    /// primera desaparecía de la barra sin que nada la hubiera resuelto.
    #[test]
    fn dos_degradaciones_conviven() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.note_degraded(degradacion_de_test("ftp", "b.org"));
        assert!(app.degraded_for("sftp").is_some());
        assert!(app.degraded_for("ftp").is_some());
        // Y la barra deja de mentir sobre cuántas hay. Nombra la ÚLTIMA y dice
        // cuántas más: un recuento pelado («2 conexiones en texto plano»), con
        // el aviso que jamás se limpia, dejaba al lector sin poder averiguar
        // NUNCA cuáles eran — y esa es la única pregunta que este indicador
        // existe para contestar.
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            banner.contains("b.org"),
            "la más reciente se nombra: {banner}"
        );
        assert!(banner.contains('1'), "y cuántas más hay: {banner}");
    }

    /// #177: «esta sesión no queda registrada» tiene que sobrevivir a la
    /// siguiente tecla. Llega UNA vez, en mitad de una operación que el usuario
    /// acaba de lanzar, y `app.message` lo borra la pulsación siguiente — que
    /// es como decir que no se avisó.
    /// #203: el ocupante SIN daemon que lo explique se dice con otra frase.
    ///
    /// El hecho es el mismo que un `Busy` —la sesión muta sin quedar
    /// registrada— y por eso el indicador sigue encendido; lo que cambia es que
    /// la frase suave sale también cuando hay un daemon vivo, o sea casi
    /// siempre, y es la que el lector ya aprendió a no mirar.
    #[test]
    fn el_ocupante_sin_daemon_tiene_su_propia_frase() {
        let mut app = App::new(Pane::new(root(), vec![]), Pane::new(root(), vec![]));
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        let soft = app.journal_banner().expect("indicador encendido");

        app.note_journal_squatted();
        let strong = app.journal_banner().expect("sigue encendido");
        assert_ne!(soft, strong, "dos hechos distintos, dos frases");

        // Y se apaga igual: una recuperación borra los dos.
        app.note_journal_recovered();
        assert!(app.journal_banner().is_none());

        // Un `Busy` posterior vuelve a la frase suave y no se queda con la
        // fuerte pegada.
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        assert_eq!(app.journal_banner().as_deref(), Some(soft.as_str()));
    }

    #[test]
    fn la_sesion_sin_journal_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.journal_banner().is_none(), "por defecto sí se registra");

        app.message = Some("algo".to_owned());
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        // Lo que borra el `message` en el run loop, tecla a tecla.
        app.message = None;
        assert!(
            app.journal_banner().is_some(),
            "el indicador no se va con el mensaje"
        );
    }

    /// Y no compite con el de #44: los dos son persistentes, de la misma clase
    /// y simultáneos, así que elegir uno escondería el otro para el resto de la
    /// sesión.
    #[test]
    fn los_dos_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        let banner = app.persistent_banner().expect("hay aviso");
        assert!(
            banner.contains("a.org"),
            "la conexión sigue nombrada: {banner}"
        );
        assert!(
            banner.starts_with(&app.journal_banner().expect("hay journal_banner")),
            "y el del journal va primero: {banner}"
        );
    }

    /// #132: la sugerencia del diálogo de empaquetar puede salir con pérdidas
    /// —el nombre del origen no siempre es UTF-8—, y confirmarla tal cual
    /// crearía un fichero con el carácter de reemplazo dentro.
    ///
    /// Dos nombres distintos que no se pueden leer dan la MISMA sugerencia, así
    /// que el segundo empaquetado chocaría contra el archivo del primero. Es el
    /// mismo rechazo, y la misma clave, que el prompt de renombrar.
    #[test]
    fn empaquetar_rehusa_un_nombre_con_el_caracter_de_reemplazo() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "caf\u{FFFD}.zip".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none(), "no se empaqueta con eso");
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("el diálogo sigue abierto para corregirlo");
        };
        assert_eq!(error.as_deref(), Some(t("msg-transfer-name-fffd").as_str()));
    }

    /// Y una extensión que norte no sabe ESCRIBIR se dice en el diálogo, en vez
    /// de empaquetar un zip con nombre de rar.
    #[test]
    fn empaquetar_rehusa_una_extension_que_no_se_escribe() {
        let mut app = app_dos_panes();
        app.modal = Some(Modal::Pack {
            name: "cosas.rar".to_owned(),
            error: None,
        });
        assert!(app.pack_confirm().is_none());
        let Some(Modal::Pack { error, .. }) = &app.modal else {
            panic!("sigue abierto");
        };
        assert!(error.is_some(), "y dice por qué");
    }

    /// #232: una ventana SUELTA lo dice una vez y luego se le olvida.
    ///
    /// El mensaje de arranque lo borra la siguiente tecla, y a partir de ahí
    /// la ventana no guarda la pantalla sin nada en pantalla que lo diga.
    #[test]
    fn la_ventana_suelta_tiene_indicador_persistente() {
        let mut app = app_dos_panes();
        assert!(app.session_banner().is_none(), "la dueña no avisa de nada");

        app.session.detached = true;
        app.message = Some("algo".to_owned());
        // Lo que borra el `message` en el run loop, tecla a tecla.
        app.message = None;
        let banner = app.persistent_banner().expect("hay aviso");
        assert_eq!(
            banner,
            app.session_banner().expect("hay session_banner"),
            "sin nada más encendido, la barra es justo ese aviso: {banner}"
        );
    }

    /// Y convive con los otros dos: son tres hechos simultáneos de la misma
    /// clase, y el de la sesión es el que menos pesa, así que va el último.
    #[test]
    fn los_tres_indicadores_persistentes_caben_juntos() {
        let mut app = app_dos_panes();
        app.note_no_journal(norte_core::embedded::NoJournal::Busy);
        app.note_degraded(degradacion_de_test("sftp", "a.org"));
        app.session.detached = true;
        let banner = app.persistent_banner().expect("hay aviso");
        assert!(
            banner.starts_with(&app.journal_banner().expect("hay journal_banner")),
            "el del journal sigue primero: {banner}"
        );
        assert!(
            banner.contains("a.org"),
            "la conexión sigue nombrada: {banner}"
        );
        assert!(
            banner.ends_with(&app.session_banner().expect("hay session_banner")),
            "y el de la sesión cierra: {banner}"
        );
    }

    /// MINOR-5: el `Option<String>` de #44 estaba acotado por construcción;
    /// una colección con clave que viene del WIRE no lo está. El tope es
    /// generoso —hay siete schemes— así que solo lo alcanza algo anómalo, y
    /// cuando pasa se tira lo más viejo y se conserva lo que acaba de llegar.
    #[test]
    fn las_degradaciones_tienen_tope() {
        let mut app = app_dos_panes();
        for i in 0..(super::DEGRADED_MAX + 10) {
            app.note_degraded(degradacion_de_test(&format!("s{i}"), "host"));
        }
        assert_eq!(app.degraded.len(), super::DEGRADED_MAX);
        assert!(
            app.degraded_for("s0").is_none(),
            "la más vieja es la que se cae"
        );
        assert!(
            app.degraded_for(&format!("s{}", super::DEGRADED_MAX + 9))
                .is_some(),
            "la última en llegar se queda"
        );
    }

    /// El host lo elige el OTRO extremo, y la barra de estado es el sitio
    /// donde llegaba crudo mientras el resto de la TUI enmascara. Un host con
    /// controles o bidi es exactamente lo que se le manda a un indicador de
    /// seguridad para que mienta.
    #[test]
    fn el_aviso_enmascara_un_host_hostil() {
        let mut app = app_dos_panes();
        app.note_degraded(degradacion_de_test("sftp", "ma\u{202e}gro.org\n"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(
            !banner.contains('\u{202e}') && !banner.contains('\n'),
            "el host llegó crudo a la barra: {banner:?}"
        );
        assert!(
            banner.contains('\u{FFFD}'),
            "y el enmascarado se VE (jamás pérdida silenciosa): {banner:?}"
        );
    }

    /// Sin degradación no hay aviso, y con UNA el aviso es el de siempre
    /// (#44): scheme y host, formateados desde el valor estructurado.
    #[test]
    fn el_aviso_de_una_sola_degradacion_nombra_la_conexion() {
        let mut app = app_dos_panes();
        assert!(app.connection_banner().is_none());
        app.note_degraded(degradacion_de_test("sftp", "remoto.example"));
        let banner = app.connection_banner().expect("hay aviso");
        assert!(banner.contains("sftp"), "{banner}");
        assert!(banner.contains("remoto.example"), "{banner}");
    }

    /// `App` con cada pane sobre SU dir (el `app_dos_panes` de arriba pone
    /// los dos sobre `root()`, que no distingue lados).
    fn app_en(left: &str, right: &str) -> App {
        App::new(
            Pane::new(vp(left), Vec::new()),
            Pane::new(vp(right), Vec::new()),
        )
    }

    /// El intercambio cruza el pane Y su historial, y deja el foco en el
    /// mismo LADO: quien miraba a la izquierda sigue mirando a la izquierda,
    /// y ahora ahí está lo que había a la derecha.
    #[test]
    fn el_intercambio_cruza_pane_e_historial_y_no_mueve_el_foco() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.history[0].record(vp("mem:///rastro-izq"));
        app.history[1].record(vp("mem:///rastro-der"));
        app.set_focus(0);

        app.swap_panes();

        assert_eq!(app.panes[0].dir(), &vp("mem:///der"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///izq"));
        assert_eq!(app.focus(), 0, "el foco se queda en su lado");
        // El rastro viaja con el CONTENIDO, no con el lado: si no, el popup
        // ofrecería llevar «atrás» a sitios donde ese contenido nunca estuvo.
        assert_eq!(
            app.history[0].entries().front(),
            Some(&vp("mem:///rastro-der"))
        );
        assert_eq!(
            app.history[1].entries().front(),
            Some(&vp("mem:///rastro-izq"))
        );
        // Y el RASTRO de atrás/adelante viaja también, no solo la MRU que
        // pinta el popup: son dos estructuras dentro del mismo `History`.
        assert_eq!(app.history[0].back_len(), 1);
        assert_eq!(
            app.history[0].step_back(vp("mem:///der")),
            Some(vp("mem:///rastro-der")),
            "el atrás del pane 0 apunta al rastro que llegó con su contenido"
        );
    }

    /// Dos intercambios son la identidad.
    #[test]
    fn dos_intercambios_dejan_todo_como_estaba() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.swap_panes();
        app.swap_panes();
        assert_eq!(app.panes[0].dir(), &vp("mem:///izq"));
        assert_eq!(app.panes[1].dir(), &vp("mem:///der"));
    }

    /// El foco se queda en el LADO también cuando estaba a la derecha: el
    /// intercambio no toca `focus` en absoluto. (Mutación de control:
    /// añadir `self.focus ^= 1` a `swap_panes` rompe aquí y en el test de
    /// arriba a la vez.)
    #[test]
    fn el_intercambio_con_el_foco_a_la_derecha_tampoco_lo_mueve() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.set_focus(1);
        app.swap_panes();
        assert_eq!(app.focus(), 1);
        assert_eq!(
            app.focused().dir(),
            &vp("mem:///izq"),
            "en el lado derecho ahora está lo que había a la izquierda"
        );
    }

    /// Popup de historial (spec 2026-07-18): navegación con `PickerAction`,
    /// Confirm devuelve el destino y cierra, Cancel cierra.
    #[test]
    fn nav_popup_historial_navega_confirma_y_cancela() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.history[0].push(vp("mem:///dos"));
        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup.as_ref().unwrap().items().len(), 2);
        assert_eq!(
            app.nav_popup.as_ref().unwrap().selected().unwrap().target,
            Some(vp("mem:///dos")),
            "más reciente primero"
        );
        assert_eq!(app.nav_popup_input(PickerAction::Down), None);
        assert_eq!(
            app.nav_popup_input(PickerAction::Confirm),
            Some(vp("mem:///uno")),
            "Confirm devuelve el destino del item resaltado"
        );
        assert!(app.nav_popup.is_none(), "Confirm cierra el popup");

        app.open_nav_popup(NavPopupKind::History);
        assert_eq!(app.nav_popup_input(PickerAction::Cancel), None);
        assert!(app.nav_popup.is_none(), "Cancel cierra el popup");
    }

    /// Un favorito INVÁLIDO (path que no parsea) se muestra con su aviso y
    /// destino `None`: Confirm sobre él es no-op (el popup sigue abierto).
    #[test]
    fn nav_popup_hotlist_item_invalido_no_confirma() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "rota".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let item = app.nav_popup.as_ref().unwrap().selected().unwrap().clone();
        assert!(item.target.is_none(), "inválida no navega");
        assert!(
            item.display.contains(&norte_i18n::t("hotlist-invalid")),
            "el aviso de inválida se pinta: {}",
            item.display
        );
        assert_eq!(app.nav_popup_input(PickerAction::Confirm), None);
        assert!(app.nav_popup.is_some(), "el popup NO se cierra");
    }

    /// `a` abre el input de nombre SOLO en hotlist; Cancel con input activo
    /// cierra el input (no el popup). `d`: el name CRUDO seleccionado sirve
    /// de clave y el borrado local refresca los items.
    #[test]
    fn nav_popup_hotlist_input_y_borrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.nav_popup_open_name_input();
        assert_eq!(
            app.nav_popup.as_ref().unwrap().name_input.as_deref(),
            Some(""),
            "`a` abre el input prellenado vacío"
        );
        app.nav_popup_input(PickerAction::Cancel);
        let p = app.nav_popup.as_ref().unwrap();
        assert!(p.name_input.is_none(), "Cancel cierra el input");
        assert!(app.nav_popup.is_some(), "…no el popup");

        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "el name CRUDO del seleccionado (clave del persist)"
        );
        app.hotlist_apply_removed("uno");
        assert_eq!(app.hotlist.len(), 1);
        let p = app.nav_popup.as_ref().unwrap();
        assert_eq!(p.items().len(), 1, "el popup se refresca tras borrar");
        assert_eq!(p.selected().unwrap().target, Some(vp("mem:///dos")));
    }

    /// review MAJOR T5: un hot-reload con el popup abierto muta
    /// `App.hotlist` mientras el usuario ve la snapshot VIEJA (items
    /// congelados a propósito) — `d` debe borrar lo MOSTRADO (clave
    /// congelada en el item), jamás lo que ahora ocupa ese índice en la
    /// lista nueva (borraría OTRO favorito: pérdida de config).
    #[test]
    fn d_con_popup_desincronizado_borra_el_mostrado() {
        let mut app = app_dos_panes();
        app.hotlist = vec![
            crate::config::HotlistItem {
                name: "uno".into(),
                target: Ok(vp("mem:///uno")),
            },
            crate::config::HotlistItem {
                name: "dos".into(),
                target: Ok(vp("mem:///dos")),
            },
        ];
        app.open_nav_popup(NavPopupKind::Hotlist);
        // Cursor en 0: el usuario VE "uno". Simula el hot-reload que quitó
        // "uno" de la config (la copia en App cambia, el popup no).
        app.hotlist.remove(0);
        assert_eq!(
            app.nav_popup_selected_hotlist_name().as_deref(),
            Some("uno"),
            "la clave es la CONGELADA del popup, no App.hotlist[cursor]"
        );
    }

    /// En el popup de HISTORIAL no hay input de nombre ni name de hotlist.
    #[test]
    fn nav_popup_historial_sin_input_ni_name() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///uno"));
        app.open_nav_popup(NavPopupKind::History);
        app.nav_popup_open_name_input();
        assert!(app.nav_popup.as_ref().unwrap().name_input.is_none());
        assert_eq!(app.nav_popup_selected_hotlist_name(), None);
    }

    /// `hotlist_apply_saved` reemplaza por name conservando posición o
    /// añade al final (misma semántica que persist/load) y refresca popup.
    #[test]
    fn hotlist_apply_saved_reemplaza_o_anade() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "uno".into(),
            target: Ok(vp("mem:///viejo")),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        app.hotlist_apply_saved("uno", vp("mem:///nuevo"));
        assert_eq!(app.hotlist.len(), 1, "reemplaza, no duplica");
        assert_eq!(app.hotlist[0].target.as_ref().unwrap(), &vp("mem:///nuevo"));
        app.hotlist_apply_saved("dos", vp("mem:///dos"));
        assert_eq!(app.hotlist.len(), 2, "name nuevo se añade al final");
        assert_eq!(
            app.nav_popup.as_ref().unwrap().items().len(),
            2,
            "el popup abierto refleja el alta"
        );
    }

    /// Un path HOSTIL en el historial sale enmascarado y con el badge como
    /// prefijo — jamás bidi/controles crudos en el popup (spec §6).
    #[test]
    fn nav_popup_sanea_paths_hostiles() {
        let mut app = app_dos_panes();
        app.history[0].push(vp("mem:///evil%E2%80%AEdir"));
        app.open_nav_popup(NavPopupKind::History);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// encoding-auditor MAJOR: `fs_type` looked like a closed, ASCII-only
    /// vocabulary (`ext4`, `nfs4`…) but a FUSE mount's `fuse.<subtype>` is
    /// the `-o subtype=` value an UNPRIVILEGED user picks (`sshfs`, `rclone
    /// mount`…) — exactly as untrusted as a filename. An earlier draft of
    /// `volume_item_display` spliced it in with `{}` and skipped
    /// `display_name` entirely, so a hostile `fs_type` reached the row raw.
    #[test]
    fn volume_row_sanea_fs_type_hostil() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: None,
            fs_type: "fuse.evil\u{202E}type".to_owned(),
            kind: norte_proto::methods::VolumeKind::Fixed,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// V3.5 (encoding-auditor MAJOR deferred from V3): `label` is
    /// `Option<Vec<u8>>` end to end now, so a non-UTF-8 label reaches this
    /// row as the ORIGINAL bytes — not a lossy `String` some earlier layer
    /// already mangled — and goes through the exact same masking `fs_type`
    /// gets above. Bytes `\xFF\xFE` are not valid UTF-8 in any position, so
    /// `display_name` must fall back to lossy rendering AND mark it hostile.
    #[test]
    fn volume_row_sanea_label_no_utf8() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(vec![0xFF, 0xFE, b'X']),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(display.starts_with('!'), "badge prefijo: {display}");
        assert!(
            display.contains('\u{FFFD}'),
            "el label no-UTF8 se pinta lossy: {display}"
        );
        assert_eq!(
            items[0].target,
            Some(vp("mem:///media/usb")),
            "el target sigue siendo el mount real, ajeno al label"
        );
    }

    /// V3.5 (encoding-auditor MINOR: the hand-picked byte string above is
    /// not the canonical corpus): every hostile name in
    /// `norte_testkit::corpus::hostile_names()`, used as a LABEL, must reach
    /// the row without panicking, badged EXACTLY when `display_name` alone
    /// says that name comes out altered — the same function
    /// `volume_item_display` calls, so this pins agreement rather than
    /// reimplementing the masking rule a second time. `target` stays the
    /// clean mount throughout: a hostile label must never leak into
    /// Enter-to-navigate.
    ///
    /// #169's `archive_marker_literal` (a label whose own CLEAN text is
    /// `"!"`, the same glyph as [`crate::ui::HOSTILE_BADGE`]) caught this
    /// assertion checking `display.starts_with('!')` — true for that
    /// fixture even with `label_hostil == false`, because the UN-badged
    /// label prefix (`"{label} — "`) itself starts with `!`. A leading `!`
    /// is not proof of a badge, and even `"! "` is not enough: that fixture's
    /// clean prefix is `"! — "`, which also starts with `"! "`. Nothing
    /// short of the FULL string settles it, so the expected display is
    /// rebuilt here from the same primitives `volume_item_display` calls
    /// (`display_name`, `path_display_with`, `t`) — not the masking rule
    /// itself, only the template it is spliced into — and compared for
    /// EXACT equality.
    #[test]
    fn volume_label_hostile_corpus_sweep() {
        let mount = vp("mem:///media/usb");
        let (path_text, path_hostile) = norte_frontend::path_display_with(&mount, None);
        assert!(
            !path_hostile,
            "control: el mount fijo del test no es hostil"
        );
        let (fs_text, fs_hostile) = display_name(b"vfat");
        assert!(!fs_hostile, "control: \"vfat\" no es hostil");
        let sizes = format!("{u} / {u}", u = t("volumes-size-unknown"));
        for fixture in norte_testkit::corpus::hostile_names() {
            let vol = norte_proto::methods::Volume {
                mount: mount.clone(),
                label: Some(fixture.bytes.clone()),
                fs_type: "vfat".to_owned(),
                kind: norte_proto::methods::VolumeKind::Removable,
                total_bytes: None,
                free_bytes: None,
                read_only: false,
            };
            let items = volume_items(std::slice::from_ref(&vol), None);
            let display = &items[0].display;
            let (label_text, label_hostile) = display_name(&fixture.bytes);
            let body = format!("{label_text} — {path_text}  {fs_text}  {sizes}");
            let expected = if label_hostile {
                format!("{} {body}", crate::ui::HOSTILE_BADGE)
            } else {
                body
            };
            assert_eq!(
                display, &expected,
                "{}: badge debe coincidir con display_name({:?})",
                fixture.id, fixture.bytes
            );
            assert_eq!(
                items[0].target,
                Some(mount.clone()),
                "{}: el target sigue siendo el mount, ajeno al label",
                fixture.id
            );
        }
    }

    /// The everyday case: an ordinary `fs_type` and absent sizes (the
    /// filesystem never answered `statvfs` in time, design §A) render with
    /// NO badge and say `volumes-size-unknown` rather than a bare zero — a
    /// zero here would read as "full", the opposite of "unknown".
    #[test]
    fn volume_row_talla_ausente_no_es_cero() {
        let vol = norte_proto::methods::Volume {
            mount: vp("mem:///media/usb"),
            label: Some(b"USB".to_vec()),
            fs_type: "vfat".to_owned(),
            kind: norte_proto::methods::VolumeKind::Removable,
            total_bytes: None,
            free_bytes: None,
            read_only: false,
        };
        let items = volume_items(std::slice::from_ref(&vol), None);
        let display = items[0].display.clone();
        assert!(!display.starts_with('!'), "nada hostil aquí: {display}");
        assert!(!display.contains('0'), "ausente no es cero: {display}");
        assert_eq!(items[0].target, Some(vp("mem:///media/usb")));
    }

    /// review MINOR T5: una entrada INVÁLIDA con name hostil también lleva
    /// el badge (antes el flag de `display_name` se descartaba en ese brazo).
    #[test]
    fn hotlist_invalida_con_name_hostil_lleva_badge() {
        let mut app = app_dos_panes();
        app.hotlist = vec![crate::config::HotlistItem {
            name: "evil\u{202E}name".into(),
            target: Err("err-invalid-path".into()),
        }];
        app.open_nav_popup(NavPopupKind::Hotlist);
        let display = app
            .nav_popup
            .as_ref()
            .unwrap()
            .selected()
            .unwrap()
            .display
            .clone();
        assert!(!display.contains('\u{202E}'), "sin bidi crudo: {display:?}");
        assert!(display.starts_with('!'), "badge prefijo: {display}");
    }

    /// TOFU (#45): confiar es decisión de seguridad — `dialog.approve`
    /// confía; `dialog.deny`/`dialog.cancel` cancelan; `dialog.confirm`
    /// (Enter) es INERTE (safety pin H1: sin default peligroso).
    #[test]
    fn trust_host_key_solo_approve_confia() {
        let m = Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: root(),
            pane: 0,
            trail: Trail::Record,
        };
        assert_eq!(
            dialog_action(&m, "dialog.approve"),
            Some(DialogOutcome::Confirmed)
        );
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.confirm"),
            None,
            "Enter (dialog.confirm) jamás confía en una host key"
        );
    }

    /// S2 (`[ui] confirm_quit`): `Modal::ConfirmQuit` reutiliza el ALLOWLIST
    /// de `ConfirmDelete`/`ConfirmTransfer` — `y`/Enter confirman (cierran),
    /// `n`/Esc cancelan, cualquier otro comando queda fuera (`None`).
    #[test]
    fn confirm_quit_reutiliza_allow_confirm() {
        let m = Modal::ConfirmQuit;
        for cmd in ["dialog.approve", "dialog.confirm"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Confirmed));
        }
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.overwrite"),
            None,
            "fuera del allowlist de confirm: inerte"
        );
    }

    /// S2 (`[ui] confirm_quit`): las tres combinaciones modo × trabajo en
    /// vuelo, cada una por separado (mismo estilo que
    /// `has_pending_work_tasks_o_marcas_o_ninguno` de la GUI).
    #[test]
    fn quit_needs_confirm_los_tres_modos() {
        use crate::config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "never NUNCA confirma, ni con trabajo en vuelo"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, false),
            "never NUNCA confirma"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "always SIEMPRE confirma, incluso sin trabajo pendiente"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, true),
            "always SIEMPRE confirma"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Auto, false),
            "auto sin trabajo pendiente: cierra directo"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "auto con trabajo pendiente: confirma (comportamiento pre-S2)"
        );
    }

    /// TOFU Lua (M4, decisión 8 del plan H1: NO migrado): mismo contrato de
    /// seguridad que el resto — solo `y` confía; `n` y Esc deniegan; Enter
    /// NO decide.
    #[test]
    fn trust_lua_init_solo_y_confia_y_enter_no_decide() {
        use crossterm::event::KeyCode as K;
        assert_eq!(trust_lua_key(K::Char('y')), DialogOutcome::Confirmed);
        assert_eq!(trust_lua_key(K::Char('n')), DialogOutcome::Cancelled);
        assert_eq!(trust_lua_key(K::Esc), DialogOutcome::Cancelled);
        assert_eq!(
            trust_lua_key(K::Enter),
            DialogOutcome::Open,
            "Enter jamás aprueba ejecutar un script ajeno"
        );
    }

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

    fn e(wire: &str, k: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse(wire).unwrap(),
            kind: k,
            size: None,
            mtime_ms: None,
        }
    }

    /// #105: F5 de UN ítem abre el nombre editable prefijado con el nombre
    /// ORIGINAL. Sin tocar, el confirm usa los BYTES crudos (regla 1: un
    /// nombre no-UTF8 copiado sin editar jamás pasa por el lossy).
    #[test]
    fn transfer_name_sin_editar_conserva_los_bytes_originales() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile.clone(),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (kind, from, dest) = app.transfer_name_confirm().expect("válido");
        assert_eq!(kind, TransferKind::Copy);
        assert_eq!(from, hostile);
        assert_eq!(
            dest,
            VPath::parse("mem:///dst")
                .unwrap()
                .join(norte_proto::Segment::new(b"informe\xFF\xFE.dat".to_vec()).unwrap()),
            "bytes crudos al destino, jamás la forma lossy"
        );
    }

    /// #105: editar sustituye el nombre por el TEXTO tecleado; y un texto
    /// que aún contiene U+FFFD (residuo del prefill lossy de un nombre
    /// hostil) se RECHAZA — confirmarlo escribiría mojibake en disco.
    #[test]
    fn transfer_name_editado_usa_el_texto_y_rechaza_fffd() {
        let dir = VPath::parse("mem:///").unwrap();
        let hostile = dir
            .clone()
            .join(norte_proto::Segment::new(b"x\xFF.dat".to_vec()).unwrap());
        let mut app = App::new(
            Pane::new(
                dir.clone(),
                vec![Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: hostile,
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                }],
            ),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        // Tocar el campo (borra el último char del prefill lossy): el texto
        // sigue llevando el U+FFFD del prefill → rechazo con diagnóstico.
        app.transfer_name_pop();
        assert!(app.transfer_name_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        // Reescrito limpio: vale, y son los bytes del texto.
        while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
            app.transfer_name_pop();
        }
        for c in "limpio.dat".chars() {
            app.transfer_name_push(c);
        }
        let (_, _, dest) = app.transfer_name_confirm().expect("limpio");
        assert_eq!(dest, VPath::parse("mem:///dst/limpio.dat").unwrap());
    }

    /// #105: shift+F6 — rename in situ: destino = MISMO dir; confirmar sin
    /// cambiar el nombre es error (no-op), y un nombre nuevo construye el
    /// destino en el propio dir.
    #[test]
    fn rename_construye_en_el_mismo_dir_y_rechaza_el_mismo_nombre() {
        let mut app = app_with_entries(&["a.txt"]);
        app.open_rename();
        assert!(
            app.transfer_name_confirm().is_none(),
            "mismo nombre = no-op, jamás un submit"
        );
        assert!(matches!(
            &app.modal,
            Some(Modal::TransferName { error: Some(_), .. })
        ));
        app.transfer_name_push('2'); // "a.txt2"
        let (kind, from, dest) = app.transfer_name_confirm().expect("nombre nuevo");
        assert_eq!(kind, TransferKind::Move);
        assert_eq!(from, VPath::parse("mem:///a.txt").unwrap());
        assert_eq!(dest, VPath::parse("mem:///a.txt2").unwrap());
    }

    /// #136: el árbol se abre anclado DONDE está el listado, no en la raíz del
    /// sistema: un árbol que colgara siempre de `/` enseñaría diez mil ramas
    /// para llegar a donde ya estás.
    #[test]
    fn el_arbol_se_ancla_donde_esta_el_listado() {
        let mut app = app_dos_panes();
        let dir = app.focused().dir().clone();
        app.toggle_tree();
        assert_eq!(app.tree().and_then(|t| t.root().cloned()), Some(dir));
        assert_eq!(app.key_owner(), KeyOwner::Tree, "se lleva el teclado");
    }

    /// **Un layout RESTAURADO con el árbol dentro trae su estado.**
    ///
    /// Es el fallo que encontró pilotar la TUI: la sesión de ayer guarda el
    /// árbol, al arrancar el hueco vuelve… y se pinta en blanco, porque el
    /// toggle que habría creado su estado no se va a pulsar — el panel ya está
    /// ahí. `set_layout` siembra el estado de CADA kind por esta razón, y el
    /// árbol tenía que entrar en esa lista.
    #[test]
    fn un_layout_con_arbol_trae_su_estado() {
        use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};

        let mut app = app_dos_panes();
        let tree = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(90), KindId::new(crate::tree::KIND)),
                Node::slot(SlotId(91), KindId::browser()),
            ],
        };
        app.set_layout(tree);
        assert!(
            app.panes.tree(SlotId(90)).is_some(),
            "el hueco del árbol llegó sin estado y se pintaría vacío"
        );
        assert!(
            app.panes.tree(SlotId(90)).and_then(|t| t.root()).is_some(),
            "y anclado en algún sitio, o no pide nada"
        );
    }

    /// Y el hueco del árbol SE COLOCA en el reparto: sin esto el layout le
    /// reserva sitio y nadie lo pinta, que es una columna en blanco.
    #[test]
    fn el_hueco_del_arbol_se_coloca() {
        use norte_frontend::layout::{KindRegistry, Rect, resolve};

        let mut app = app_dos_panes();
        app.toggle_tree();
        let id = app.tree_slot().expect("abierto");
        let res = resolve(
            Rect::new(0, 0, 110, 30),
            &app.layout,
            &KindRegistry::builtin(),
        );
        assert!(
            res.placements.iter().any(|(p, _)| *p == id),
            "el hueco del árbol no se colocó: {:?}",
            res.placements
        );
        assert!(app.panes.tree(id).is_some(), "y su panel está");
    }

    /// Tres pulsaciones, como el sidebar: abre y enfoca, vuelve a enfocar,
    /// cierra. La del medio es la que hace que soltar el teclado no cierre el
    /// panel.
    #[test]
    fn el_arbol_abre_enfoca_y_cierra() {
        let mut app = app_dos_panes();
        app.toggle_tree();
        assert!(app.tree_slot().is_some());
        app.return_keys_to_panes();
        app.toggle_tree();
        assert!(
            app.tree_slot().is_some(),
            "la segunda solo recupera el teclado"
        );
        assert_eq!(app.key_owner(), KeyOwner::Tree);
        app.toggle_tree();
        assert!(app.tree_slot().is_none(), "y la tercera cierra");
        assert_eq!(app.key_owner(), KeyOwner::Panes);
    }

    /// #139: las propiedades salen del LISTADO, y sobre una carpeta piden lo
    /// único que el listado no sabe.
    #[test]
    fn las_propiedades_de_una_carpeta_piden_contarla() {
        let mut app = app_with_entries(&["a.txt"]);
        // Sobre un fichero no hay nada que contar: su tamaño ya está.
        assert!(app.open_properties().is_none());
        assert!(matches!(app.modal, Some(Modal::Properties { .. })));
    }

    /// El resultado de un recuento va al diálogo que lo pidió, y a NINGÚN
    /// otro: entre abrir el diálogo y que termine la cuenta cabe otra cuenta
    /// —la que el humano lanzó sobre una selección— y enseñar ese número aquí
    /// sería contestar otra pregunta.
    #[test]
    fn el_recuento_ajeno_no_entra_en_el_dialogo() {
        use norte_proto::TaskId;

        let mut app = app_with_entries(&["a.txt"]);
        app.open_properties();
        let mine = TaskId::new(7);
        app.properties_counting(mine);
        assert!(
            !app.properties_sized(TaskId::new(8), 1, 1),
            "el de otro no entra"
        );
        assert!(app.properties_sized(mine, 4096, 12), "el mío sí");
        let Some(Modal::Properties { size, .. }) = &app.modal else {
            panic!("sigue abierto")
        };
        assert_eq!(*size, Some((4096, 12)));
    }

    /// Sin diálogo abierto, un recuento no tiene dónde entrar y lo dice: es lo
    /// que hace que el run loop mande el número a la barra de estado.
    #[test]
    fn sin_dialogo_el_recuento_no_encuentra_donde_ir() {
        let mut app = app_with_entries(&["a.txt"]);
        assert!(!app.properties_sized(norte_proto::TaskId::new(1), 10, 1));
    }

    /// #138: la tecla de orden hace lo mismo que un click en la cabecera —
    /// invierte si ya está activa, ordena ascendente si es nueva— y SOLO sobre
    /// el panel con el foco: el orden es de un listado, como el cursor.
    #[test]
    fn una_tecla_de_orden_solo_toca_el_panel_con_el_foco() {
        use norte_frontend::{SortColumn, SortDir};

        let mut app = app_dos_panes();
        let other = app.panes[1].sort();
        app.sort_focused_by(SortColumn::Size);
        assert_eq!(app.focused().sort().column, SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Asc,
            "una nueva, ascendente"
        );
        assert_eq!(app.panes[1].sort(), other, "el otro panel no se entera");

        app.sort_focused_by(SortColumn::Size);
        assert_eq!(
            app.focused().sort().dir,
            SortDir::Desc,
            "la misma otra vez invierte"
        );
        app.sort_focused_by(SortColumn::Extension);
        assert_eq!(app.focused().sort().column, SortColumn::Extension);
        assert_eq!(app.focused().sort().dir, SortDir::Asc);
    }

    /// Y `dirs_first` no lo toca ninguna tecla de orden: es una preferencia,
    /// no un criterio de columna.
    #[test]
    fn una_tecla_de_orden_no_toca_los_directorios_primero() {
        use norte_frontend::SortColumn;

        let mut app = app_dos_panes();
        let mut spec = app.focused().sort();
        spec.dirs_first = false;
        app.focused_mut().set_sort(spec);
        app.sort_focused_by(SortColumn::Mtime);
        assert!(!app.focused().sort().dirs_first);
    }

    /// #108 b4: `apply_scheme_sort` aplica el orden de la config al pane
    /// según su scheme — el hook de cd y el arranque pasan por aquí.
    #[test]
    fn apply_scheme_sort_ordena_por_la_config() {
        use norte_frontend::columns::ColumnsSettings;
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str, size: Option<u64>| {
            let mut e = e(&format!("mem:///{n}"), EntryKind::File);
            e.size = size;
            e
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a", Some(3)), mk("b", Some(1))]),
            Pane::new(dir, Vec::new()),
        );
        let cfg = norte_config::ColumnsConfig {
            default_columns: None,
            sort: Some(norte_config::SortChoice {
                column: norte_config::SortColumnKey::Size,
                descending: false,
                dirs_first: true,
            }),
            schemes: std::collections::BTreeMap::new(),
            ..Default::default()
        };
        app.columns = ColumnsSettings::resolve(&cfg);
        app.apply_scheme_sort(0);
        let order: Vec<_> = app.panes[0]
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                VPath::parse("mem:///b").unwrap(),
                VPath::parse("mem:///a").unwrap()
            ],
            "size asc desde la config"
        );
    }

    /// #105 review MAJOR-1: el submit de UN ítem que vino de la MARCA la
    /// CONSUME (doctrina mc/TC del lote); un rename (cursor) jamás toca
    /// las marcas, y Esc tampoco.
    #[test]
    fn el_submit_de_un_item_consume_la_marca_y_el_rename_no() {
        let dir = VPath::parse("mem:///").unwrap();
        let mk = |n: &str| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(norte_proto::Segment::new(n.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(dir.clone(), vec![mk("a"), mk("b")]),
            Pane::new(VPath::parse("mem:///dst").unwrap(), Vec::new()),
        );
        app.focused_mut().toggle_mark(); // marca "a"
        app.focused_mut().move_down(1); // cursor en "b"
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let (_, from, _) = app.transfer_name_confirm().expect("válido");
        assert_eq!(
            from,
            VPath::parse("mem:///a").unwrap(),
            "la MARCA, no el cursor"
        );
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 0, "el envío consume la marca");

        // Esc no consume.
        app.focused_mut().toggle_mark(); // marca "b" (cursor sigue ahí)
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.cancel_transfer_name();
        assert_eq!(app.focused().marks_len(), 1, "cancelar conserva la marca");

        // Rename (cursor) no toca marcas ajenas.
        app.open_rename();
        app.transfer_name_push('2');
        assert!(app.transfer_name_confirm().is_some());
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 1, "el rename no consume marcas");
    }

    /// #105 (regla 1, corpus canónico): renombrar un nombre hostil a uno
    /// limpio conserva el `from` BYTE-EXACTO para cada nombre del corpus —
    /// el origen jamás pasa por texto, solo el nombre nuevo es tecleado.
    #[test]
    fn rename_de_cada_nombre_hostil_del_corpus_conserva_el_from() {
        let dir = VPath::parse("mem:///").unwrap();
        for (i, hostile) in norte_testkit::corpus::hostile_names().iter().enumerate() {
            let from = dir
                .clone()
                .join(norte_proto::Segment::new(hostile.bytes.clone()).unwrap());
            let mut app = App::new(
                Pane::new(
                    dir.clone(),
                    vec![Entry {
                        attrs: std::collections::BTreeMap::new(),
                        path: from.clone(),
                        kind: EntryKind::File,
                        size: None,
                        mtime_ms: None,
                    }],
                ),
                Pane::new(dir.clone(), Vec::new()),
            );
            app.open_rename();
            while matches!(&app.modal, Some(Modal::TransferName { name, .. }) if !name.is_empty()) {
                app.transfer_name_pop();
            }
            for c in "limpio".chars() {
                app.transfer_name_push(c);
            }
            let (_, got_from, dest) = app
                .transfer_name_confirm()
                .unwrap_or_else(|| panic!("corpus[{i}] {}", hostile.id));
            assert_eq!(got_from, from, "corpus[{i}]: from byte-exacto");
            assert_eq!(dest, VPath::parse("mem:///limpio").unwrap());
        }
    }

    /// #104: el modal de F7 valida con las reglas del `VPath` y devuelve el
    /// destino completo; inválido = diagnóstico en el modal, jamás submit.
    #[test]
    fn el_modal_mkdir_valida_y_construye_el_destino() {
        let mut app = app_with_entries(&["a"]);
        app.open_mkdir();
        for c in "docs".chars() {
            app.mkdir_push(c);
        }
        let target = app.mkdir_confirm().expect("nombre válido");
        assert_eq!(target, VPath::parse("mem:///docs").unwrap());
        assert!(
            app.modal.is_some(),
            "confirmar NO cierra: cierra el submit que encoló (MINOR-1)"
        );
        // Un submit fallido deja el diagnóstico y conserva el nombre…
        app.mkdir_set_error("policy".into());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), name }) if name == "docs"
        ));
        // …y el que encoló, cierra.
        app.mkdir_submitted();
        assert!(app.modal.is_none(), "submitted cierra el modal");

        // Vacío: error, modal abierto.
        app.open_mkdir();
        assert!(app.mkdir_confirm().is_none());
        assert!(
            matches!(&app.modal, Some(Modal::Mkdir { error: Some(_), .. })),
            "el diagnóstico queda en el modal"
        );

        // `..` es DotSegment: jamás un destino.
        app.mkdir_push('.');
        app.mkdir_push('.');
        assert!(app.mkdir_confirm().is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::Mkdir { error: Some(_), .. })
        ));

        // `/` embebido: InvalidByte.
        app.cancel_mkdir();
        app.open_mkdir();
        for c in "a/b".chars() {
            app.mkdir_push(c);
        }
        assert!(app.mkdir_confirm().is_none());

        // Cancelar cierra sin nada.
        app.cancel_mkdir();
        assert!(app.modal.is_none());
    }

    /// #107 review MAJOR-1: los hits de una búsqueda son EXPLÍCITOS — el
    /// pane virtual suspende el filtro de ocultos. Con `[ui] show_hidden =
    /// false`, buscar "env" DEBE enseñar `.env`: tragárselo en silencio
    /// (mientras el contador de hits decía 1 sobre un pane vacío) era el
    /// bug. Al volver a un listado real, la preferencia vuelve a mandar.
    #[test]
    fn el_pane_virtual_de_busqueda_ensena_hits_ocultos() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        p.set_show_hidden(false); // seed de config: ocultar
        assert_eq!(p.entries().len(), 1, "el listado real filtra");

        p.begin_search(VPath::parse("mem:///").unwrap());
        p.extend_listing(vec![e("mem:///sub/.env", EntryKind::File)]);
        assert_eq!(
            p.entries().len(),
            1,
            "el hit oculto ES visible en el pane virtual"
        );

        // Ctrl+H dentro del pane virtual filtra los RESULTADOS…
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 0);

        // …pero NO toca la preferencia: el listado real vuelve filtrando
        // (y un toggle en el real sí la cambia).
        p.set_listing(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///.env", EntryKind::File),
                e("mem:///a", EntryKind::File),
            ],
        );
        assert_eq!(p.entries().len(), 1, "la preferencia (ocultar) manda");
        p.toggle_hidden();
        assert_eq!(p.entries().len(), 2, "toggle real: mostrar");
        p.begin_listing(
            VPath::parse("mem:///sub").unwrap(),
            vec![
                e("mem:///sub/.git", EntryKind::Dir),
                e("mem:///sub/x", EntryKind::File),
            ],
            false,
            None,
        );
        assert_eq!(
            p.entries().len(),
            2,
            "begin_listing respeta la preferencia nueva (mostrar)"
        );
    }

    /// #103 review MAJOR-4: `Pane` delega la API de marcas en `PaneState`
    /// sin reimplementar nada — pero el set de partida debe ser ASIMÉTRICO
    /// en cada paso, o `mark_all`/`invert_marks`/`clear_marks` quedan
    /// indistinguibles entre sí (p. ej. sobre un set vacío, `mark_all` e
    /// `invert_marks` dan el mismo resultado). Cada aserción de abajo
    /// falsaría si esa llamada se sustituyera por CUALQUIER otra delegada.
    #[test]
    fn pane_delegates_the_mark_api() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
                e("mem:///c", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        let c = p.entries()[2].clone();

        // toggle_mark: marca SOLO la entrada bajo el cursor ("a").
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a) && !p.is_marked(&b) && !p.is_marked(&c));

        // mark_all desde {a}: las TRES, incluida "a" — si esto llamara a
        // invert_marks en su lugar, "a" se desmarcaría y el total sería 2.
        p.mark_all();
        assert_eq!(p.marks_len(), 3);
        assert!(p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // Reset a un set asimétrico de nuevo para poder distinguir invert.
        p.clear_marks();
        p.toggle_mark(); // {a}

        // invert_marks desde {a}: exactamente LAS OTRAS DOS — ni el set
        // vacío que daría clear_marks, ni las tres que daría mark_all.
        p.invert_marks();
        assert_eq!(p.marks_len(), 2);
        assert!(!p.is_marked(&a) && p.is_marked(&b) && p.is_marked(&c));

        // clear_marks desde {b, c}: vacío — invert_marks aquí daría {a}
        // (marks_len 1), mark_all daría 3.
        p.clear_marks();
        assert_eq!(p.marks_len(), 0);
    }

    /// El dispatch real de `mark.toggle` (main.rs) es una ÚNICA llamada a
    /// `toggle_mark_and_advance` (#103 review MAJOR-2: la composición
    /// "marca + avanza" ya no se parte en dos llamadas del dispatch —
    /// vive entera en el modelo compartido, que decide avanzar dentro del
    /// filtro o el cursor real; ver
    /// `norte_frontend::pane::tests::toggle_mark_and_advance_stays_inside_an_active_filter`
    /// para el caso con filtro). `dispatch` en sí no es testeable aquí sin
    /// un daemon real (pide `&Backend`/`&mut EventStream`), así que este
    /// test pinea la misma llamada al nivel de `Pane`: marca Y avanza, y en
    /// la última fila no envuelve.
    #[test]
    fn mark_toggle_advances_without_wrapping_at_the_end() {
        let mut p = Pane::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                e("mem:///a", EntryKind::File),
                e("mem:///b", EntryKind::File),
            ],
        );
        let a = p.entries()[0].clone();
        let b = p.entries()[1].clone();
        assert_eq!(p.cursor(), 0);

        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&a), "la fila 0 quedó marcada");
        assert_eq!(p.cursor(), 1, "el cursor avanzó tras marcar");

        // Última fila: togglear + avanzar NO debe envolver a 0.
        p.toggle_mark_and_advance();
        assert_eq!(p.marks_len(), 2);
        assert!(p.is_marked(&b), "la fila 1 (última) también quedó marcada");
        assert_eq!(p.cursor(), 1, "clampado en la última fila, no envuelve");
    }

    /// App de un solo listado de nombres, sobre `mem://` (task 9, #103):
    /// vía `pane_con` — mismo orden real que pinta la UI — con foco en el
    /// pane lleno; el otro vacío. Nombre distinto de `app_with_sized_entries`
    /// (`tests/status_marks.rs`): esa lleva tamaño explícito, esta solo
    /// nombres.
    fn app_with_entries(names: &[&str]) -> App {
        App::new(pane_con(names), Pane::new(root(), Vec::new()))
    }

    /// Como [`app_with_entries`], con el pane INACTIVO plantado en `dst`
    /// (vacío): el destino ortodoxo de F5/F6 es el DIRECTORIO del otro pane
    /// (#103 T10), así que los tests del lote necesitan un destino distinto
    /// de la raíz de origen.
    fn app_with_two_panes(names: &[&str], dst: &str) -> App {
        App::new(
            pane_con(names),
            Pane::new(VPath::parse(dst).unwrap(), Vec::new()),
        )
    }

    /// #103 T10: F5/F6 construyen el modal desde TODAS las marcas, y el
    /// destino es el DIRECTORIO del otro pane (con varios ítems no hay un
    /// nombre único que editar — eso es #105).
    #[test]
    fn copy_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3, "las tres quedaron marcadas");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::ConfirmTransfer { items, to, .. }) = &app.modal else {
            panic!("no transfer modal");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(to, &VPath::parse("mem:///dst").unwrap());
    }

    /// Sin ninguna marca, F5 sigue operando sobre el CURSOR (el gesto
    /// clásico no se pierde) — `marked_paths` cae al seleccionado. Con UN
    /// solo ítem la puerta abre el nombre EDITABLE (#105), no el confirm de
    /// lista: es la misma decisión para la tecla y para un drop.
    #[test]
    fn copy_without_marks_still_uses_the_cursor_entry() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        assert_eq!(app.focused().marks_len(), 0, "sin marcas de partida");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        let Some(Modal::TransferName {
            from,
            to_dir,
            from_marks,
            ..
        }) = &app.modal
        else {
            panic!("no transfer modal");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///a").unwrap(),
            "marked_paths falls back to the cursor"
        );
        assert_eq!(to_dir, &VPath::parse("mem:///dst").unwrap());
        assert!(!from_marks, "no había marca que consumir");
    }

    /// Un arrastre PROMOVIDO lleva la fila del press y NADA más: ni las
    /// marcas del pane (que son otra cosa que el usuario no ha soltado) ni
    /// su consumo al enviar. La promoción cambia lo que el gesto HACE, no lo
    /// que está seleccionado.
    #[test]
    fn a_promoted_transfer_carries_one_row_and_does_not_consume_the_marks() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 3);
        app.open_transfer(TransferKind::Copy, 0, 1, Some(2));
        let Some(Modal::TransferName {
            from, from_marks, ..
        }) = &app.modal
        else {
            panic!("un solo ítem: nombre editable");
        };
        assert_eq!(
            from,
            &VPath::parse("mem:///c").unwrap(),
            "la fila promovida, no las tres marcas"
        );
        assert!(!from_marks, "el envío NO puede consumir las marcas");
        app.transfer_name_submitted();
        assert_eq!(app.focused().marks_len(), 3, "las marcas siguen ahí");
    }

    /// Un índice promovido que ya no nombra ninguna fila (el listado encogió
    /// entre el gesto y el drop) no abre nada: jamás un diálogo sobre un
    /// lote vacío, y jamás cayendo hacia las marcas —que sería copiar lo que
    /// nadie arrastró—.
    #[test]
    fn a_promoted_index_out_of_range_opens_nothing() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_transfer(TransferKind::Copy, 0, 1, Some(9));
        assert!(app.modal.is_none());
    }

    /// Las marcas las CONSUME el ENVÍO del lote (mc/Total Commander): tras
    /// `consume_marks` no queda una selección a medio consumir.
    #[test]
    fn submitting_a_bulk_operation_consumes_the_marks() {
        let mut app = app_with_two_panes(&["a", "b"], "mem:///dst");
        app.focused_mut().mark_all();
        assert_eq!(app.focused().marks_len(), 2, "marcadas antes de enviar");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        app.consume_marks();
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// F8 sobre las marcas: el modal lleva el lote entero y el modo
    /// (papelera/permanente) que decidió el caller tras sondear la
    /// capability UNA vez.
    #[test]
    fn delete_builds_the_modal_from_every_mark() {
        let mut app = app_with_two_panes(&["a", "b", "c"], "mem:///dst");
        app.focused_mut().mark_all();
        app.open_delete_modal(true);
        let Some(Modal::ConfirmDelete { items, permanent }) = &app.modal else {
            panic!("no delete modal");
        };
        assert_eq!(items.len(), 3);
        assert!(*permanent);
    }

    /// Un pane VACÍO no abre modal: no hay nada que copiar ni que borrar
    /// (ni marcas ni cursor) — jamás un diálogo sobre un lote vacío.
    #[test]
    fn an_empty_pane_opens_no_bulk_modal() {
        let mut app = app_with_two_panes(&[], "mem:///dst");
        app.open_transfer(TransferKind::Copy, 0, 1, None);
        assert!(app.modal.is_none(), "sin ítems no hay modal de copia");
        app.open_delete_modal(false);
        assert!(app.modal.is_none(), "sin ítems no hay modal de borrado");
    }

    /// #103 T9: el modal de patrón marca/desmarca y reporta cuántas marcas
    /// cambió — camino feliz (glob válido, matches reales).
    #[test]
    fn the_pattern_modal_marks_and_reports_how_many() {
        let mut app = app_with_entries(&["a.rs", "b.rs", "c.txt"]);
        app.open_mark_pattern(true);
        assert!(matches!(
            app.modal,
            Some(Modal::MarkPattern { mark: true, .. })
        ));
        app.mark_pattern_push('*');
        app.mark_pattern_push('.');
        app.mark_pattern_push('r');
        app.mark_pattern_push('s');
        let changed = app.mark_pattern_confirm().expect("valid glob");
        assert_eq!(changed, 2);
        assert!(app.modal.is_none());
        assert_eq!(app.focused().marks_len(), 2);
    }

    /// Un patrón inválido (glob que no compila) deja el modal ABIERTO con el
    /// diagnóstico — el usuario conserva lo tecleado para corregirlo — y no
    /// marca nada.
    #[test]
    fn an_invalid_pattern_keeps_the_modal_open_and_marks_nothing() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('[');
        assert!(app.mark_pattern_confirm().is_err());
        assert!(app.modal.is_some(), "the user keeps their text to fix it");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// #103 T9 review MINOR: `Modal::MarkPattern` no tiene ALLOWLIST — es
    /// texto libre, el run loop lo intercepta ANTES del contexto `dialog`
    /// (main.rs). Esto pinea la mitad de seguridad de esa afirmación:
    /// NINGÚN comando del vocabulario `dialog.*`, ni siquiera
    /// `dialog.confirm` (Enter), puede confirmarlo a través de
    /// `dialog_action` — si alguna vez este modal se colara al contexto
    /// `dialog` por un bug de enrutado, seguiría siendo inerte ahí.
    #[test]
    fn dialog_action_es_siempre_none_para_mark_pattern() {
        let m = Modal::MarkPattern {
            mark: true,
            pattern: String::new(),
            error: None,
        };
        for cmd in crate::keymap::DIALOG_COMMANDS {
            assert_eq!(
                dialog_action(&m, cmd),
                None,
                "{cmd} no debe confirmar/cancelar MarkPattern vía dialog_action"
            );
        }
    }

    /// Cancelar (`cancel_mark_pattern`, el Esc de este modal de texto libre)
    /// no marca nada, aunque el usuario ya hubiera tecleado un patrón — y
    /// cierra el modal, la propiedad real que este test debía pinear.
    #[test]
    fn the_pattern_modal_cancels_without_marking() {
        let mut app = app_with_entries(&["a.rs"]);
        app.open_mark_pattern(true);
        app.mark_pattern_push('*');
        app.cancel_mark_pattern();
        assert!(app.modal.is_none(), "cancel closes the modal");
        assert_eq!(app.focused().marks_len(), 0);
    }

    /// Review rust MAJOR M1: `cancel_mark_pattern` NO es un cierre genérico
    /// — con un `Modal::ApproveAgentOp` abierto (llegado, p. ej., mientras
    /// el usuario tecleaba un patrón que luego se sustituyó), debe dejarlo
    /// INTACTO. Cerrarlo sin el `policy.decide(approve: false)` async que
    /// hace `on_dialog_key` dejaría al agente sin respuesta hasta el TTL
    /// del daemon, y al humano sin volver a ver la pregunta.
    ///
    /// El guard es un `debug_assert!`: en ESTE build (test = dev,
    /// `debug-assertions` activas) panica ANTES de tocar `self.modal` — se
    /// captura con `catch_unwind` para poder comprobar el estado posterior
    /// en la misma aserción; en release sería un no-op y la función
    /// devolvería temprano igual, mismo resultado sobre el modal.
    #[test]
    fn cancel_mark_pattern_leaves_an_approval_modal_untouched() {
        let mut app = app_with_entries(&["a.rs"]);
        let approval = Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 7,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///proj/a".into(), "mem:///proj/b".into()],
                paths_total: 0,
                ttl_ms: 60_000,
            },
        };
        app.modal = Some(approval.clone());
        // Silencia el hook de pánico por defecto: el panic se captura y se
        // espera, no debe ensuciar la salida de este test con un backtrace.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.cancel_mark_pattern();
        }));
        std::panic::set_hook(prev_hook);
        assert!(
            result.is_err(),
            "el guard debe panicar en debug ante el mal uso"
        );
        assert_eq!(
            app.modal,
            Some(approval),
            "an approval modal must not be closeable without a decision"
        );
    }
}

#[cfg(test)]
mod error_message_tests {
    use super::{
        DETAIL_MAX_CHARS, KeymapsError, config_error_category, detail_for_bar, error_message,
        io_error_category, keymaps_error_category, theme_error_category,
    };
    use crate::config;
    use norte_proto::{ConflictKind, Error};

    /// Cada categoría rinde un mensaje LOCALIZADO propio — jamás el `Display`
    /// inglés hardcodeado del proto (#20, spec §17.7).
    #[test]
    fn cada_categoria_tiene_mensaje_propio_no_display() {
        let _ = norte_i18n::force(norte_i18n::Lang::Es);
        let nf = error_message(&Error::NotFound);
        assert!(nf.contains("no encontrado"), "localizado ES: {nf}");
        assert!(
            !nf.contains("not found"),
            "NO es el Display inglés del proto: {nf}"
        );
        // Las variantes de Conflict se distinguen entre sí.
        let exists = error_message(&Error::Conflict {
            conflict: ConflictKind::Exists,
        });
        let case = error_message(&Error::Conflict {
            conflict: ConflictKind::CaseCollision,
        });
        assert_ne!(exists, case, "cada ConflictKind rinde distinto");
        // PolicyDenied jamás filtra la regla concreta (vocabulario cerrado).
        let pd = error_message(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(
            !pd.contains("scope-expired"),
            "la regla concreta NO se muestra: {pd}"
        );
    }

    /// Una categoría futura desconocida cae a `err-unknown`, jamás vacía.
    #[test]
    fn categoria_desconocida_cae_a_unknown() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let u = error_message(&Error::Unknown);
        assert!(u.contains("unknown error"), "{u}");
    }

    /// `error_category` (la base de TODOS los renders de la barra) jamás
    /// filtra el host de un `HostKeyUnknown` — un host hostil con override
    /// bidi sería un spoof de la barra — ni la `rule` de un `PolicyDenied`.
    #[test]
    fn categoria_no_filtra_host_hostil_ni_rule() {
        use super::error_category;
        let hk = error_category(&Error::HostKeyUnknown {
            host: "evil\u{202E}host".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
        });
        assert!(!hk.contains("evil"), "el host NO se muestra: {hk:?}");
        assert!(!hk.contains('\u{202E}'), "sin bidi en la barra: {hk:?}");
        let pd = error_category(&Error::PolicyDenied {
            rule: "scope-expired".into(),
        });
        assert!(!pd.contains("scope-expired"), "la regla NO se filtra: {pd}");
    }

    /// La clave ESTABLE (`error_key`, la que ven los scripts Lua) y el texto
    /// localizado (`error_category`) son el MISMO mapa: cero duplicación,
    /// cero deriva entre lo que compara un script y lo que pinta la barra.
    #[test]
    fn error_key_es_la_clave_estable_de_la_categoria() {
        use super::{error_category, error_key};
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        assert_eq!(error_key(&Error::NotFound), "err-not-found");
        assert_eq!(error_key(&Error::Cancelled), "err-cancelled");
        assert_eq!(error_key(&Error::Unknown), "err-unknown");
        // 0.36.0: los dos brazos del batch de renames viven sobre un
        // `_ => "err-unknown"`, así que borrarlos COMPILA y la suite seguiría
        // verde — con dos errores que su rustdoc llama accionables cayendo en
        // «error desconocido», que es lo contrario. Estos asserts son lo único
        // que lo impide.
        assert_eq!(error_key(&Error::PlanStale), "err-plan-stale");
        assert_eq!(
            error_key(&Error::PlanNotExecutable),
            "err-plan-not-executable"
        );
        // La categoría es exactamente t(clave).
        assert_eq!(
            error_category(&Error::NotFound),
            norte_i18n::t("err-not-found")
        );
    }

    /// 0.40.0: las tres relaciones de solape se pintan DISTINTO, y ninguna
    /// cae en «error desconocido».
    ///
    /// Vive sobre el mismo `_ => "err-unknown"` que los dos de arriba, así que
    /// borrar el brazo compila y deja al lector con «error desconocido» ante la
    /// única negativa de esta familia que se arregla moviéndose de sitio — que
    /// es exactamente lo que la variante existe para no ser. Y las tres claves
    /// tienen que ser tres: la frase accionable de `Same` («elige otro
    /// directorio») no es la de las otras dos («sal del árbol que contiene al
    /// otro»).
    #[test]
    fn cada_relacion_de_solape_tiene_su_propia_frase() {
        use super::{error_category, error_key};
        use norte_proto::RootOverlap;
        let mut seen = std::collections::BTreeSet::new();
        for relation in [
            RootOverlap::Same,
            RootOverlap::SourceInsideDest,
            RootOverlap::DestInsideSource,
        ] {
            let key = error_key(&Error::OverlappingRoots { relation });
            assert!(
                key.starts_with("err-overlapping-roots"),
                "{relation:?} → {key}"
            );
            assert!(seen.insert(key), "dos relaciones comparten {key}");
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let text = norte_i18n::t_in(lang, key);
                assert_ne!(text, key, "{key} sin traducir en {lang:?}");
                assert_ne!(
                    text,
                    norte_i18n::t_in(lang, "err-unknown"),
                    "{key} dice lo mismo que «error desconocido»"
                );
                assert_ne!(
                    text,
                    norte_i18n::t_in(lang, "err-internal"),
                    "{key} dice lo mismo que «error interno»"
                );
            }
        }
        // Y una relación de un protocolo más nuevo cae en la clave GENÉRICA de
        // la familia, no en `err-unknown`: sigue siendo un solape.
        assert_eq!(
            error_key(&Error::OverlappingRoots {
                relation: RootOverlap::Unknown
            }),
            "err-overlapping-roots"
        );
        assert_eq!(
            error_category(&Error::OverlappingRoots {
                relation: RootOverlap::Same
            }),
            norte_i18n::t("err-overlapping-roots-same")
        );
    }

    /// #73: un error LOCAL de io va por categoría Fluent — jamás el
    /// `Display` del OS («Permission denied (os error 13)», que el SO
    /// localiza a su antojo — regla 1).
    #[test]
    fn categoria_io_local_no_filtra_el_display_del_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let s = io_error_category(&e);
        assert!(s.contains("permission denied"), "{s}");
        assert!(!s.contains("os error"), "sin string del OS: {s}");
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let s = io_error_category(&full);
        assert!(s.contains("no space"), "kind con clave propia: {s}");
    }

    /// #73: `ConfigError` rinde categoría localizada + path; el diagnóstico
    /// del parser se conserva (la posición es lo accionable) pero pasa por
    /// `display_name` — jamás bidi/controles crudos en la barra — y con tope.
    #[test]
    fn categoria_config_no_filtra_el_diagnostico_del_parser() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = config::ConfigError::Toml {
            path: "/etc/norte/config.toml".into(),
            message: "unknown field `colr\u{202E}` at line 3".into(),
        };
        let s = config_error_category(&e);
        assert!(s.contains("config.toml"), "el path SÍ se muestra: {s}");
        assert!(s.contains("line 3"), "la posición es lo accionable: {s}");
        assert!(!s.contains('\u{202E}'), "sin bidi en la barra: {s}");
        let e = config::ConfigError::Io {
            path: "/etc/norte/config.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let s = config_error_category(&e);
        assert!(s.contains("permission denied"), "io por categoría: {s}");
        assert!(!s.contains("os error"), "sin string del OS: {s}");
    }

    /// #73 (ALTA-1 del encoding-auditor): el pipeline de TEMAS tenía el
    /// mismo bug — spec hostil (puede venir del `./.norte` de un repo AJENO)
    /// y Display del OS, crudos a la barra vía `apply_theme`.
    #[test]
    fn categoria_tema_no_filtra_spec_hostil_ni_os() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let e = crate::theme::ResolveError::Io {
            spec: "temas/\u{202E}x.toml".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let s = theme_error_category(&e);
        assert!(!s.contains("os error"), "sin string del OS: {s}");
        assert!(!s.contains('\u{202E}'), "sin bidi en la barra: {s}");
        assert!(s.contains("not found"), "io por categoría: {s}");
        let e = crate::theme::ResolveError::Parse {
            spec: "nord".into(),
            detail: "role `panel\u{202E}` desconocido".into(),
        };
        let s = theme_error_category(&e);
        assert!(s.contains("nord"), "el spec saneado sí se muestra: {s}");
        assert!(!s.contains('\u{202E}'), "detalle enmascarado: {s}");
    }

    /// #73: un diagnóstico kilométrico (un TOML hostil puede citar valores
    /// arbitrarios) sale RECORTADO — la barra es una línea.
    #[test]
    fn el_detalle_del_parser_tiene_tope() {
        let s = detail_for_bar(&"x".repeat(1000));
        assert!(s.chars().count() <= DETAIL_MAX_CHARS + 1, "{}", s.len());
        assert!(s.ends_with('…'), "recorte marcado: {s}");
    }

    /// #73: el error de keymaps se localiza por Fluent (los contextos anyhow
    /// castellanos hardcodeados violaban la convención de i18n).
    #[test]
    fn error_de_keymap_se_localiza_con_el_nombre_del_preset() {
        let _ = norte_i18n::force(norte_i18n::Lang::En);
        let s = keymaps_error_category(&KeymapsError::UnknownPreset {
            name: "vintage".into(),
            available: "cua, orthodox".into(),
        });
        assert!(s.contains("vintage"), "el nombre pedido es accionable: {s}");
        assert!(s.contains("cua, orthodox"), "y los disponibles: {s}");
        assert!(
            !s.contains("preset desconocido"),
            "sin castellano fijo: {s}"
        );
        let s = keymaps_error_category(&KeymapsError::Invalid {
            detail: "conflicto en F5".into(),
        });
        assert!(s.contains("invalid keymap"), "localizado: {s}");
        assert!(s.contains("F5"), "el detalle diagnóstico se conserva: {s}");
    }
}
