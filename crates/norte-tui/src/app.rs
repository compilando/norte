//! Estado puro del TUI (panes, cursor, presentación de nombres) y la
//! presentación de ERRORES para la barra (#73): categorías Fluent
//! ([`error_key`]/[`error_category`] y compañía) + saneado de detalle
//! ([`detail_for_bar`]). Máquina testeable sin terminal — el render (`ui`)
//! y el I/O (`main`) viven aparte; los scripts Lua (M4) consumen de aquí la
//! clave ESTABLE de [`error_key`].

use norte_i18n::{Lang, t, ta};
use norte_proto::{Error, VPath};

#[cfg(test)]
pub(crate) mod testutil;

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
    use super::testutil::*;
    use super::*;

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
